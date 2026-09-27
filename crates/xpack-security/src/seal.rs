//! Sealing a package with a password, and opening it again.
//!
//! A publisher who wants an application installed only by people who know a
//! password seals its packages: the installer's, every update, every delta.
//! A sealed file is the signed package, encrypted. Opening it gives back the
//! package byte for byte, whose signature is then checked exactly as an
//! unsealed one's is. Sealing hides the package; the signature still decides
//! whether to trust it.
//!
//! # The key
//!
//! Derived from the password with Argon2id, deliberately slow and memory-hard,
//! so guessing costs something even though nothing limits how often an
//! attacker holding the file can try. The settings are fixed for each format
//! version rather than written in each file: an installation keeps the key,
//! never the password, and opens every later update with it, so the key must
//! come out the same for every file of the application. For the same reason
//! the salt is derived from the application's id rather than drawn at random:
//! the same password gives two applications different keys, and one
//! application the same key every time.
//!
//! # The file
//!
//! ```text
//! magic "XPACKSLD" (8) | format (4, big-endian) | id length (1) | application id
//!   | nonce prefix (19) | chunks…
//! ```
//!
//! The application's id is in the clear: it is what the key is derived for, so
//! a program given a sealed file and a password must be able to read it
//! first. It is not a secret; the installer shows it before asking.
//!
//! The package is encrypted with XChaCha20-Poly1305 in the STREAM
//! construction: 1 MiB chunks, each with its own tag, numbered, and the last
//! marked as last. So a chunk cannot be changed, moved, repeated or dropped,
//! and the file cannot be cut short, without opening failing. The header is
//! authenticated with every chunk, so it cannot be changed either. The nonce
//! prefix is random for every file, which is what lets one key seal many.

use std::io::{Read, Write};
use std::path::Path;

use aead_stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
use xpack_core::{Error, Result};
use zeroize::Zeroizing;

/// The first bytes of every sealed file.
pub const MAGIC: [u8; 8] = *b"XPACKSLD";

/// The format this build writes and reads.
pub const FORMAT: u32 = 1;

/// Plaintext bytes in each chunk but the last.
pub const CHUNK: usize = 1024 * 1024;

/// Bytes of the random nonce prefix; the stream adds a 4-byte counter and a
/// last-chunk flag to make the cipher's 24.
const NONCE_PREFIX: usize = 19;

/// Bytes of each chunk's tag.
const TAG: usize = 16;

/// Bytes of the header before the application id.
const FIXED: usize = MAGIC.len() + 4 + 1;

/// Argon2id's memory, in KiB, for format 1.
const MEMORY_KIB: u32 = 64 * 1024;
/// Argon2id's passes, for format 1.
const PASSES: u32 = 3;

/// The shortest password a publisher may seal with.
///
/// Nothing slows an attacker holding a sealed file but the key derivation, so
/// a short password can be guessed offline whatever it is derived with.
pub const MIN_PASSWORD_CHARS: usize = 12;

/// The key a password gives an application. Wiped from memory when dropped.
pub struct SealKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey(..)")
    }
}

impl SealKey {
    /// Derives the key `password` gives the application `application_id`.
    ///
    /// Takes about a second, on purpose.
    pub fn derive(password: &str, application_id: &str) -> Result<Self> {
        let params = argon2::Params::new(MEMORY_KIB, PASSES, 1, Some(32))
            .map_err(|e| Error::invalid("password", e.to_string()))?;
        let argon =
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        let salt = crate::sha256(
            format!("xpack sealed package, format {FORMAT}\0{application_id}").as_bytes(),
        );
        let mut key = Zeroizing::new([0u8; 32]);
        argon
            .hash_password_into(password.as_bytes(), &salt.as_bytes()[..16], key.as_mut())
            .map_err(|e| Error::invalid("password", e.to_string()))?;
        Ok(Self(key))
    }

    /// The key as an installation keeps it: hex.
    pub fn to_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(self.0.as_ref()))
    }

    /// A key an installation kept.
    pub fn from_hex(text: &str) -> Result<Self> {
        let bytes = Zeroizing::new(
            hex::decode(text.trim()).map_err(|_| Error::invalid("sealing key", "is not hex"))?,
        );
        let mut key = Zeroizing::new([0u8; 32]);
        if bytes.len() != key.len() {
            return Err(Error::invalid("sealing key", "is not 32 bytes"));
        }
        key.copy_from_slice(&bytes);
        Ok(Self(key))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        let key: &[u8; 32] = &self.0;
        XChaCha20Poly1305::new(key.into())
    }
}

/// Refuses a password too short to be worth sealing with.
pub fn ensure_strong_enough(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(Error::invalid(
            "password",
            format!("must be at least {MIN_PASSWORD_CHARS} characters"),
        ));
    }
    Ok(())
}

/// Whether the file at `path` is sealed, from its first bytes.
pub fn is_sealed(path: &Path) -> Result<bool> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut magic = [0u8; MAGIC.len()];
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(magic == MAGIC),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Seals the file at `input`, the package of `application_id`, into `output`
/// with the key for that application.
pub fn seal(input: &Path, output: &Path, application_id: &str, key: &SealKey) -> Result<()> {
    let id = application_id.as_bytes();
    let id_len = u8::try_from(id.len())
        .map_err(|_| Error::invalid("sealing", "the application id is too long"))?;
    let mut prefix = [0u8; NONCE_PREFIX];
    getrandom::fill(&mut prefix)
        .map_err(|e| Error::invalid("sealing", format!("no randomness: {e}")))?;
    let mut header = Vec::with_capacity(FIXED + id.len() + NONCE_PREFIX);
    header.extend_from_slice(&MAGIC);
    header.extend_from_slice(&FORMAT.to_be_bytes());
    header.push(id_len);
    header.extend_from_slice(id);
    header.extend_from_slice(&prefix);

    let result = (|| {
        let mut reader = std::fs::File::open(input).map_err(|e| Error::io(input, e))?;
        let mut writer = std::io::BufWriter::new(
            std::fs::File::create(output).map_err(|e| Error::io(output, e))?,
        );
        writer.write_all(&header).map_err(|e| Error::io(output, e))?;

        let mut stream = EncryptorBE32::from_aead(key.cipher(), (&prefix).into());
        let mut chunk = read_up_to(&mut reader, CHUNK, input)?;
        loop {
            let next = read_up_to(&mut reader, CHUNK, input)?;
            if next.is_empty() {
                let sealed = stream
                    .encrypt_last(aead_payload(&chunk, &header))
                    .map_err(|_| Error::invalid("sealing", "the cipher refused a chunk"))?;
                writer.write_all(&sealed).map_err(|e| Error::io(output, e))?;
                break;
            }
            let sealed = stream
                .encrypt_next(aead_payload(&chunk, &header))
                .map_err(|_| Error::invalid("sealing", "the cipher refused a chunk"))?;
            writer.write_all(&sealed).map_err(|e| Error::io(output, e))?;
            chunk = next;
        }
        writer.flush().map_err(|e| Error::io(output, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// The header of a sealed file: what it was sealed for, and the bytes every
/// chunk is authenticated with.
struct Header {
    bytes: Vec<u8>,
    application_id: String,
    prefix: [u8; NONCE_PREFIX],
}

fn read_header(reader: &mut impl Read, path: &Path) -> Result<Header> {
    let mut fixed = [0u8; FIXED];
    reader.read_exact(&mut fixed).map_err(|_| not_sealed(path))?;
    if fixed[..MAGIC.len()] != MAGIC {
        return Err(not_sealed(path));
    }
    let mut format = [0u8; 4];
    format.copy_from_slice(&fixed[MAGIC.len()..MAGIC.len() + 4]);
    let format = u32::from_be_bytes(format);
    if format != FORMAT {
        return Err(Error::UnsupportedFormatVersion { found: format, supported: FORMAT });
    }
    let mut id = vec![0u8; usize::from(fixed[FIXED - 1])];
    reader.read_exact(&mut id).map_err(|_| not_sealed(path))?;
    let mut prefix = [0u8; NONCE_PREFIX];
    reader.read_exact(&mut prefix).map_err(|_| not_sealed(path))?;
    let application_id = String::from_utf8(id.clone()).map_err(|_| not_sealed(path))?;
    let mut bytes = fixed.to_vec();
    bytes.extend_from_slice(&id);
    bytes.extend_from_slice(&prefix);
    Ok(Header { bytes, application_id, prefix })
}

/// The application a sealed file was sealed for, which is what its key is
/// derived for.
pub fn application_of(path: &Path) -> Result<String> {
    let mut reader = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    Ok(read_header(&mut reader, path)?.application_id)
}

/// Opens the sealed file at `input` into `output` with `key`.
///
/// Nothing is left in `output` unless every chunk opened: a wrong key, or a
/// file changed, reordered or cut short anywhere, leaves no file behind.
pub fn open(input: &Path, output: &Path, key: &SealKey) -> Result<()> {
    let result = (|| {
        let mut reader = std::fs::File::open(input).map_err(|e| Error::io(input, e))?;
        let header = read_header(&mut reader, input)?;

        let mut writer = std::io::BufWriter::new(
            std::fs::File::create(output).map_err(|e| Error::io(output, e))?,
        );
        let mut stream = DecryptorBE32::from_aead(key.cipher(), (&header.prefix).into());
        let mut chunk = read_up_to(&mut reader, CHUNK + TAG, input)?;
        loop {
            let next = read_up_to(&mut reader, CHUNK + TAG, input)?;
            if next.is_empty() {
                let opened = stream
                    .decrypt_last(aead_payload(&chunk, &header.bytes))
                    .map_err(|_| refused())?;
                writer.write_all(&opened).map_err(|e| Error::io(output, e))?;
                break;
            }
            let opened =
                stream.decrypt_next(aead_payload(&chunk, &header.bytes)).map_err(|_| refused())?;
            writer.write_all(&opened).map_err(|e| Error::io(output, e))?;
            chunk = next;
        }
        writer.flush().map_err(|e| Error::io(output, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

fn aead_payload<'a>(msg: &'a [u8], aad: &'a [u8]) -> chacha20poly1305::aead::Payload<'a, 'a> {
    chacha20poly1305::aead::Payload { msg, aad }
}

/// Reads up to `limit` bytes, fewer only at the end of the file.
fn read_up_to(reader: &mut impl Read, limit: usize, path: &Path) -> Result<Vec<u8>> {
    let mut buffer = Vec::with_capacity(limit);
    reader.by_ref().take(limit as u64).read_to_end(&mut buffer).map_err(|e| Error::io(path, e))?;
    Ok(buffer)
}

fn not_sealed(path: &Path) -> Error {
    Error::invalid("sealed package", format!("{} is not a sealed package", path.display()))
}

/// Opening failed: which of the causes is not something the cipher can tell.
fn refused() -> Error {
    Error::Integrity(
        "the sealed package did not open: the password is wrong, or the file was damaged or \
         changed"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "correct horse battery staple";

    fn key() -> SealKey {
        SealKey::derive(PASSWORD, "com.example.app").unwrap()
    }

    /// A plaintext of `len` bytes, varied so every chunk differs.
    fn plain(dir: &Path, len: usize) -> std::path::PathBuf {
        let path = dir.join("plain");
        let bytes: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn sealed(dir: &Path, len: usize, key: &SealKey) -> std::path::PathBuf {
        let path = dir.join("sealed");
        seal(&plain(dir, len), &path, "com.example.app", key).unwrap();
        path
    }

    /// Bytes of the header these tests seal with.
    const HEADER_LEN: usize = FIXED + "com.example.app".len() + NONCE_PREFIX;

    #[test]
    fn a_sealed_file_opens_to_exactly_what_was_sealed() {
        let key = key();
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK + 17] {
            let dir = tempfile::tempdir().unwrap();
            let sealed = sealed(dir.path(), len, &key);
            assert!(is_sealed(&sealed).unwrap());
            assert_eq!(application_of(&sealed).unwrap(), "com.example.app");
            let opened = dir.path().join("opened");
            open(&sealed, &opened, &key).unwrap();
            assert_eq!(
                std::fs::read(&opened).unwrap(),
                std::fs::read(dir.path().join("plain")).unwrap(),
                "{len} bytes"
            );
        }
    }

    #[test]
    fn the_same_password_gives_one_application_one_key_and_two_applications_two() {
        let a = SealKey::derive(PASSWORD, "com.example.app").unwrap();
        let again = SealKey::derive(PASSWORD, "com.example.app").unwrap();
        let other = SealKey::derive(PASSWORD, "com.example.other").unwrap();
        assert_eq!(*a.to_hex(), *again.to_hex());
        assert_ne!(*a.to_hex(), *other.to_hex());
        assert_eq!(*SealKey::from_hex(&a.to_hex()).unwrap().to_hex(), *a.to_hex());
    }

    #[test]
    fn the_wrong_password_opens_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sealed = sealed(dir.path(), 3 * CHUNK, &key());
        let wrong = SealKey::derive("not the password at all", "com.example.app").unwrap();
        let opened = dir.path().join("opened");

        let err = open(&sealed, &opened, &wrong).unwrap_err();

        assert!(err.to_string().contains("password is wrong"), "{err}");
        assert!(!opened.exists(), "something was left behind");
    }

    /// Changes `sealed` with `change`, and checks it no longer opens.
    fn refused_after(change: impl FnOnce(&mut Vec<u8>)) {
        let dir = tempfile::tempdir().unwrap();
        let key = key();
        let sealed = sealed(dir.path(), 3 * CHUNK + 5, &key);
        let mut bytes = std::fs::read(&sealed).unwrap();
        change(&mut bytes);
        std::fs::write(&sealed, &bytes).unwrap();
        let opened = dir.path().join("opened");
        assert!(open(&sealed, &opened, &key).is_err(), "a changed file opened");
        assert!(!opened.exists(), "something was left behind");
    }

    #[test]
    fn a_changed_byte_in_a_middle_chunk_is_refused() {
        refused_after(|bytes| bytes[HEADER_LEN + CHUNK + TAG + 100] ^= 1);
    }

    #[test]
    fn two_chunks_swapped_are_refused() {
        refused_after(|bytes| {
            let one = HEADER_LEN..HEADER_LEN + CHUNK + TAG;
            let two = HEADER_LEN + CHUNK + TAG..HEADER_LEN + 2 * (CHUNK + TAG);
            let first: Vec<u8> = bytes[one.clone()].to_vec();
            let second: Vec<u8> = bytes[two.clone()].to_vec();
            bytes[one].copy_from_slice(&second);
            bytes[two].copy_from_slice(&first);
        });
    }

    #[test]
    fn a_file_cut_short_at_a_chunk_boundary_is_refused() {
        // Without the last-chunk flag, dropping the end would still open.
        refused_after(|bytes| bytes.truncate(HEADER_LEN + 3 * (CHUNK + TAG)));
    }

    #[test]
    fn a_changed_header_is_refused() {
        // The application id: a file claiming another application's name.
        refused_after(|bytes| bytes[FIXED] ^= 1);
    }

    #[test]
    fn a_file_that_is_not_sealed_is_said_to_be_so() {
        let dir = tempfile::tempdir().unwrap();
        let plain = plain(dir.path(), 100);
        assert!(!is_sealed(&plain).unwrap());
        assert!(
            open(&plain, &dir.path().join("o"), &key())
                .unwrap_err()
                .to_string()
                .contains("not a sealed")
        );
    }

    #[test]
    fn nothing_of_the_package_is_readable_in_the_sealed_file() {
        let dir = tempfile::tempdir().unwrap();
        let secret = b"THE-APPLICATION'S-OWN-SECRET-MARKER";
        let path = dir.path().join("plain");
        std::fs::write(&path, secret.repeat(1000)).unwrap();
        let sealed = dir.path().join("sealed");
        seal(&path, &sealed, "com.example.app", &key()).unwrap();
        let bytes = std::fs::read(&sealed).unwrap();
        assert!(!bytes.windows(secret.len()).any(|w| w == secret), "the plaintext shows through");
    }

    #[test]
    fn a_short_password_is_refused() {
        assert!(ensure_strong_enough("short").is_err());
        assert!(ensure_strong_enough(PASSWORD).is_ok());
    }
}
