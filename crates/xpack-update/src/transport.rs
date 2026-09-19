//! Fetching bytes from an update server.
//!
//! The trait exists so the update engine can be tested against hostile
//! responses. Mocking the HTTP client would test the mock; putting the seam
//! here means a test supplies bytes and the engine's own handling is what runs.

use std::io::{Read, Write};

use xpack_core::{Error, Result};

/// Somewhere update artefacts can be fetched from.
pub trait UpdateTransport {
    /// Streams `url` into `sink`, refusing to write more than `limit` bytes.
    ///
    /// Returns how many bytes were written. Implementations must enforce
    /// `limit` **while streaming**, not by checking a declared length
    /// afterwards.
    fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64>;

    /// Streams `url` into memory, bounded by `limit`.
    fn fetch_to_vec(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        self.fetch(url, limit, &mut buffer)?;
        Ok(buffer)
    }
}

/// Copy buffer size.
const CHUNK: usize = 64 * 1024;

/// Fetches over HTTPS.
#[cfg(feature = "https")]
#[derive(Debug)]
pub struct HttpsTransport {
    agent: ureq::Agent,
}

#[cfg(feature = "https")]
impl Default for HttpsTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "https")]
impl HttpsTransport {
    /// Builds a transport with conservative timeouts.
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(300)))
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            // Redirects are followed, because object stores and CDNs rely on
            // them, but the final URL's scheme is checked below. Allowing an
            // unbounded chain would let a server keep a client busy forever.
            .max_redirects(5)
            .build()
            .into();
        Self { agent }
    }
}

/// Rejects a URL that is not HTTPS.
///
/// Checked here, at the point the request is made, rather than relying on
/// validation that happened elsewhere on input this function cannot see.
/// Loopback over plain HTTP is permitted so a test can run a throwaway server
/// without weakening the rule that real deployments use TLS.
pub fn ensure_secure(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    let loopback = url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost")
        || url.starts_with("http://[::1]");
    if loopback {
        return Ok(());
    }
    Err(Error::Transport(format!("{url} is not an https:// URL")))
}

#[cfg(feature = "https")]
impl UpdateTransport for HttpsTransport {
    fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64> {
        ensure_secure(url)?;

        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| Error::Transport(format!("fetching {url}: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Transport(format!("{url} returned HTTP {status}")));
        }

        // A redirect chain must not end somewhere unencrypted. Checking only
        // the URL we asked for would let a server downgrade the transport by
        // bouncing the client to http://.
        {
            use ureq::http::response::Response as HttpResponse;
            let _: &HttpResponse<_> = &response;
            let final_uri = ureq::ResponseExt::get_uri(&response).to_string();
            ensure_secure(&final_uri).map_err(|_| {
                Error::Transport(format!("{url} redirected to {final_uri}, which is not https"))
            })?;
        }

        let mut reader = response.into_body().into_reader();
        stream_bounded(&mut reader, sink, limit, url)
    }
}

/// Copies at most `limit` bytes, aborting the moment that is exceeded.
///
/// The declared `Content-Length` is deliberately ignored. A hostile server can
/// claim any length and then stream indefinitely, so the only bound that means
/// anything is the one enforced on bytes actually received.
pub fn stream_bounded(
    reader: &mut impl Read,
    sink: &mut dyn Write,
    limit: u64,
    subject: &str,
) -> Result<u64> {
    let mut buffer = vec![0u8; CHUNK];
    let mut total: u64 = 0;

    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| Error::Transport(format!("reading {subject}: {e}")))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > limit {
            return Err(Error::Transport(format!(
                "{subject} exceeded its {limit} byte limit; the server is sending more than it \
                 declared"
            )));
        }
        sink.write_all(&buffer[..read])
            .map_err(|e| Error::Transport(format!("writing {subject}: {e}")))?;
    }
    Ok(total)
}
