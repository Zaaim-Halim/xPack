//! Reporting download progress without disturbing the transport.

use std::io::Write;

use xpack_core::progress::{ProgressEvent, ProgressReporter};

/// How often progress is reported, in bytes.
///
/// A report per 64 KiB chunk would be thousands of updates for a large
/// package, most of them redrawing the same percentage. Reporting on a
/// boundary keeps a terminal readable and a GUI responsive without either
/// doing pointless work.
const REPORT_INTERVAL: u64 = 256 * 1024;

/// Wraps a writer, reporting bytes as they are written.
///
/// Deliberately a writer rather than a change to
/// [`crate::transport::UpdateTransport::fetch`]. That signature is the seam the
/// hostile-server tests are built on, and leaving it alone means those tests
/// keep exercising exactly what they were written to exercise.
pub struct ProgressWriter<'a> {
    inner: &'a mut dyn Write,
    progress: &'a dyn ProgressReporter,
    total_bytes: Option<u64>,
    written: u64,
    last_reported: u64,
}

impl<'a> ProgressWriter<'a> {
    /// Wraps `inner`, reporting to `progress`.
    ///
    /// `total_bytes` is what the update server claimed, so it may be wrong or
    /// absent; it is passed through unchanged for a consumer to decide what to
    /// render.
    pub fn new(
        inner: &'a mut dyn Write,
        progress: &'a dyn ProgressReporter,
        total_bytes: Option<u64>,
    ) -> Self {
        Self { inner, progress, total_bytes, written: 0, last_reported: 0 }
    }

    /// Bytes written so far.
    pub fn written(&self) -> u64 {
        self.written
    }
}

impl Write for ProgressWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.written = self.written.saturating_add(written as u64);

        if self.written - self.last_reported >= REPORT_INTERVAL {
            self.last_reported = self.written;
            self.progress.report(&ProgressEvent::DownloadProgress {
                downloaded: self.written,
                total_bytes: self.total_bytes,
            });
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // A final report, so the last partial interval is not lost and a bar
        // does not finish at ninety-something percent.
        self.progress.report(&ProgressEvent::DownloadProgress {
            downloaded: self.written,
            total_bytes: self.total_bytes,
        });
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<ProgressEvent>>);

    impl ProgressReporter for Recorder {
        fn report(&self, event: &ProgressEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn reports_as_bytes_are_written() {
        let recorder = Recorder::default();
        let mut sink = Vec::new();
        {
            let mut writer = ProgressWriter::new(&mut sink, &recorder, Some(1024 * 1024));
            for _ in 0..8 {
                writer.write_all(&vec![0u8; 128 * 1024]).unwrap();
            }
            writer.flush().unwrap();
            assert_eq!(writer.written(), 1024 * 1024);
        }
        let events = recorder.0.lock().unwrap();
        assert!(!events.is_empty(), "progress must be reported during a download");
        assert!(
            matches!(events.last(), Some(ProgressEvent::DownloadProgress { downloaded, .. }) if *downloaded == 1024 * 1024),
            "the final report must show the whole download: {events:?}"
        );
    }

    #[test]
    fn does_not_report_on_every_chunk() {
        // Thousands of redundant updates make a terminal unreadable and a GUI
        // busy for nothing.
        let recorder = Recorder::default();
        let mut sink = Vec::new();
        {
            let mut writer = ProgressWriter::new(&mut sink, &recorder, Some(1024 * 1024));
            for _ in 0..256 {
                writer.write_all(&vec![0u8; 4096]).unwrap();
            }
        }
        let events = recorder.0.lock().unwrap();
        assert!(events.len() < 20, "expected coalesced reports, got {}", events.len());
    }

    #[test]
    fn passes_an_unknown_total_through_unchanged() {
        // A server that declares nothing must not become a total of zero.
        let recorder = Recorder::default();
        let mut sink = Vec::new();
        {
            let mut writer = ProgressWriter::new(&mut sink, &recorder, None);
            writer.write_all(&vec![0u8; 512 * 1024]).unwrap();
        }
        let events = recorder.0.lock().unwrap();
        assert!(
            matches!(
                events.first(),
                Some(ProgressEvent::DownloadProgress { total_bytes: None, .. })
            ),
            "got {events:?}"
        );
    }

    #[test]
    fn every_byte_still_reaches_the_underlying_writer() {
        let recorder = Recorder::default();
        let mut sink = Vec::new();
        {
            let mut writer = ProgressWriter::new(&mut sink, &recorder, Some(9));
            writer.write_all(b"unchanged").unwrap();
        }
        assert_eq!(sink, b"unchanged");
    }
}
