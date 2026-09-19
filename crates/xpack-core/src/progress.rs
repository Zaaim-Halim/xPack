//! Observing a long-running operation.
//!
//! An update can take minutes. Something has to be able to tell a user what is
//! happening — a splash screen, a progress bar, a status line in a desktop
//! application — and none of those can poll a function that has not returned
//! yet.
//!
//! The engine emits [`ProgressEvent`]s and takes no view on how they are shown.
//! A consumer implements [`ProgressReporter`] and renders however it likes;
//! anything that wants text for free can use [`ProgressEvent::message`] rather
//! than inventing its own vocabulary for the same states.
//!
//! # Reporters must not fail, and must not panic
//!
//! [`ProgressReporter::report`] returns nothing, because showing progress is
//! never a reason to fail an installation. It must also not panic: it is called
//! from inside a download, and unwinding through that skips the cleanup that
//! removes a partial file.

use std::fmt;
use std::io::Write;
use std::sync::Mutex;

use serde::Serialize;

use crate::Version;

/// Something that watches an operation as it runs.
///
/// `Send + Sync` because an operation may report from whichever thread it
/// happens to be on, and a consumer may render on another. The method takes
/// `&self`, so a reporter needing mutable state holds it behind a lock.
pub trait ProgressReporter: Send + Sync {
    /// Called as the operation advances.
    ///
    /// Must not panic. See the module documentation.
    fn report(&self, event: &ProgressEvent);
}

/// A reporter that discards everything.
///
/// The default, so every existing call site keeps working unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl ProgressReporter for NoProgress {
    fn report(&self, _event: &ProgressEvent) {}
}

/// Something that happened during an operation.
///
/// Marked `non_exhaustive`: new stages will be added, and doing so must not
/// break a consumer that already matches on the ones it knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum ProgressEvent {
    /// Asking the update server what it has.
    CheckingForUpdate {
        /// Application being checked.
        application: String,
    },
    /// The installation is already current.
    UpToDate {
        /// Version in use.
        version: Version,
    },
    /// A newer version exists.
    UpdateAvailable {
        /// Version offered.
        version: Version,
        /// Size the server claims. See the note on totals below.
        total_bytes: Option<u64>,
    },
    /// A download has begun.
    DownloadStarted {
        /// Version being fetched.
        version: Version,
        /// Size the server claims, when it gave a usable one.
        ///
        /// `None` means render an indeterminate indicator. This number comes
        /// from the update server and is not trustworthy: a server declaring
        /// one byte and sending ten megabytes must not produce a bar at
        /// 1000 %. The download is bounded regardless; this is only about not
        /// displaying nonsense.
        total_bytes: Option<u64>,
    },
    /// More of the download has arrived.
    DownloadProgress {
        /// Bytes received so far.
        downloaded: u64,
        /// Claimed total, with the same caveat as above.
        total_bytes: Option<u64>,
    },
    /// The download finished.
    DownloadCompleted {
        /// Bytes actually received, which is the number that is true.
        bytes: u64,
    },
    /// Checking the package's signature and hashes.
    Verifying {
        /// Version being verified.
        version: Version,
    },
    /// Writing the payload into the installation.
    Installing {
        /// Version being installed.
        version: Version,
    },
    /// More of the payload has been extracted and verified.
    ExtractionProgress {
        /// Files written so far.
        files_completed: usize,
        /// Files the signed manifest declares.
        files_total: usize,
        /// Bytes written so far.
        bytes_completed: u64,
        /// Bytes the signed manifest declares.
        bytes_total: u64,
    },
    /// Making the new version the active one.
    Activating {
        /// Version being activated.
        version: Version,
    },
    /// The operation finished successfully.
    Completed {
        /// Version now installed.
        version: Version,
    },
    /// The new version failed and the previous one was restored.
    RolledBack {
        /// Version abandoned.
        from: Version,
        /// Version restored.
        to: Version,
    },
    /// The operation failed.
    Failed {
        /// What went wrong, already formatted for a person.
        reason: String,
    },
}

impl ProgressEvent {
    /// A human-readable line describing this event.
    ///
    /// Provided so every consumer shows the same words for the same state. A
    /// splash screen, a log line and a terminal should not each invent their
    /// own phrasing for "verifying".
    pub fn message(&self) -> String {
        match self {
            Self::CheckingForUpdate { application } => {
                format!("Checking {application} for updates")
            }
            Self::UpToDate { version } => format!("Already up to date ({version})"),
            Self::UpdateAvailable { version, total_bytes } => match total_bytes {
                Some(bytes) => format!("Update available: {version} ({})", format_bytes(*bytes)),
                None => format!("Update available: {version}"),
            },
            Self::DownloadStarted { version, .. } => format!("Downloading {version}"),
            Self::DownloadProgress { downloaded, total_bytes } => match total_bytes {
                Some(total) if *total > 0 => {
                    format!("Downloading {} of {}", format_bytes(*downloaded), format_bytes(*total))
                }
                _ => format!("Downloading {}", format_bytes(*downloaded)),
            },
            Self::DownloadCompleted { bytes } => format!("Downloaded {}", format_bytes(*bytes)),
            Self::Verifying { version } => format!("Verifying {version}"),
            Self::Installing { version } => format!("Installing {version}"),
            Self::ExtractionProgress { files_completed, files_total, .. } => {
                format!("Extracting {files_completed} of {files_total} files")
            }
            Self::Activating { version } => format!("Activating {version}"),
            Self::Completed { version } => format!("Updated to {version}"),
            Self::RolledBack { from, to } => format!("{from} failed; restored {to}"),
            Self::Failed { reason } => format!("Failed: {reason}"),
        }
    }

    /// Returns `true` when no further events will follow.
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed { .. } | Self::UpToDate { .. })
    }
}

impl fmt::Display for ProgressEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// The version of the event stream's shape.
///
/// Carried on every line rather than announced once, so a consumer that
/// attaches to a stream already in progress, or reads a single line out of a
/// log, still knows what it is looking at.
pub const STREAM_SCHEMA: u32 = 1;

/// Writes one JSON object per line, for a program to read.
///
/// # Why a stream and not a window
///
/// A GUI application already has a toolkit, a theme, a language and a place
/// its users expect progress to appear. xPack drawing its own window would
/// mean a second, foreign-looking dialog that cannot be localised or themed to
/// match — and it would put a windowing stack inside a security-critical
/// binary that is otherwise free of one and cross-compiles to musl.
///
/// So the engine reports and the application renders. The application spawns
/// the updater, reads its standard output a line at a time, and drives its own
/// progress dialog. That works from Java, .NET, Python, Electron or anything
/// else that can read a pipe, which is the same runtime independence the rest
/// of xPack is built on.
///
/// # Format
///
/// One object per line, newline-terminated, flushed as it is written so a
/// reader sees each event as it happens rather than when a buffer fills.
///
/// ```text
/// {"v":1,"event":"downloadProgress","message":"Downloading 1.0 MiB of 46.0 MiB",
///  "downloaded":1048576,"totalBytes":48210432}
/// ```
///
/// `event` is the discriminator and the remaining fields belong to that
/// variant. `message` is always present and always the same wording every
/// other consumer uses, so an application that only wants a status line does
/// not have to compose one.
///
/// A reader must **ignore events it does not recognise**: stages are added
/// over time, and [`ProgressEvent`] is `non_exhaustive` for the same reason.
pub struct JsonProgress<W: Write + Send> {
    writer: Mutex<W>,
}

impl<W: Write + Send> JsonProgress<W> {
    /// Writes the stream to `writer`.
    pub fn new(writer: W) -> Self {
        Self { writer: Mutex::new(writer) }
    }
}

impl JsonProgress<std::io::Stdout> {
    /// Writes the stream to standard output, where a parent process reads it.
    ///
    /// Standard output rather than standard error because this is the
    /// process's result, not its diagnostics: a caller redirecting one and not
    /// the other should get the machine-readable half here.
    pub fn to_stdout() -> Self {
        Self::new(std::io::stdout())
    }
}

impl<W: Write + Send> ProgressReporter for JsonProgress<W> {
    fn report(&self, event: &ProgressEvent) {
        // Every failure here is swallowed deliberately. A closed pipe means the
        // application stopped listening, which is not a reason to fail an
        // update that is otherwise working — and this runs inside a download,
        // where a panic would skip the cleanup that removes the partial file.
        let Ok(mut writer) = self.writer.lock() else {
            return;
        };
        let Ok(serde_json::Value::Object(mut fields)) = serde_json::to_value(event) else {
            return;
        };
        fields.insert("v".to_string(), STREAM_SCHEMA.into());
        fields.insert("message".to_string(), event.message().into());

        if let Ok(line) = serde_json::to_string(&fields) {
            let _ = writeln!(writer, "{line}");
            // Flushed per event: a progress stream buffered until the end is
            // not a progress stream.
            let _ = writer.flush();
        }
    }
}

/// Formats a byte count for a person reading it.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn every_event_describes_itself_without_placeholders() {
        let events = [
            ProgressEvent::CheckingForUpdate { application: "com.example.app".into() },
            ProgressEvent::UpToDate { version: v("1.0.0") },
            ProgressEvent::UpdateAvailable { version: v("1.1.0"), total_bytes: Some(2048) },
            ProgressEvent::UpdateAvailable { version: v("1.1.0"), total_bytes: None },
            ProgressEvent::DownloadStarted { version: v("1.1.0"), total_bytes: Some(2048) },
            ProgressEvent::DownloadProgress { downloaded: 512, total_bytes: Some(2048) },
            ProgressEvent::DownloadProgress { downloaded: 512, total_bytes: None },
            ProgressEvent::DownloadCompleted { bytes: 2048 },
            ProgressEvent::Verifying { version: v("1.1.0") },
            ProgressEvent::Installing { version: v("1.1.0") },
            ProgressEvent::ExtractionProgress {
                files_completed: 3,
                files_total: 9,
                bytes_completed: 10,
                bytes_total: 90,
            },
            ProgressEvent::Activating { version: v("1.1.0") },
            ProgressEvent::Completed { version: v("1.1.0") },
            ProgressEvent::RolledBack { from: v("1.1.0"), to: v("1.0.0") },
            ProgressEvent::Failed { reason: "disk full".into() },
        ];
        for event in &events {
            let message = event.message();
            assert!(!message.is_empty(), "{event:?} has no message");
            assert!(!message.contains('{'), "unformatted placeholder in {message:?}");
            assert_eq!(message, event.to_string(), "Display must match message()");
        }
    }

    #[test]
    fn a_download_with_no_declared_total_still_reads_sensibly() {
        // A server that declares nothing must not produce "512 B of 0 B".
        let event = ProgressEvent::DownloadProgress { downloaded: 512, total_bytes: None };
        assert_eq!(event.message(), "Downloading 512 B");

        // Nor must one that declares zero.
        let zero = ProgressEvent::DownloadProgress { downloaded: 512, total_bytes: Some(0) };
        assert_eq!(zero.message(), "Downloading 512 B");
    }

    #[test]
    fn terminal_events_are_marked_as_such() {
        assert!(ProgressEvent::Completed { version: v("1.0.0") }.is_final());
        assert!(ProgressEvent::Failed { reason: "x".into() }.is_final());
        assert!(ProgressEvent::UpToDate { version: v("1.0.0") }.is_final());
        assert!(!ProgressEvent::Verifying { version: v("1.0.0") }.is_final());
    }

    #[test]
    fn byte_counts_are_scaled_for_reading() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
    }

    #[test]
    fn the_default_reporter_accepts_everything_and_does_nothing() {
        NoProgress.report(&ProgressEvent::Verifying { version: v("1.0.0") });
    }
}

#[cfg(test)]
mod json_stream_tests {
    use super::*;

    /// Captures the stream into a buffer a test can read back.
    #[derive(Clone, Default)]
    struct Sink(std::sync::Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn lines_for(events: &[ProgressEvent]) -> Vec<serde_json::Value> {
        let sink = Sink::default();
        let reporter = JsonProgress::new(sink.clone());
        for event in events {
            reporter.report(event);
        }
        let bytes = sink.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line must be a JSON object"))
            .collect()
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn every_event_is_one_self_describing_line() {
        // A consumer reads a line at a time, so an event spanning two lines or
        // two events sharing one would both break parsing.
        let events = [
            ProgressEvent::CheckingForUpdate { application: "com.example.demo".into() },
            ProgressEvent::UpdateAvailable { version: v("1.1.0"), total_bytes: Some(48_210_432) },
            ProgressEvent::DownloadProgress {
                downloaded: 1_048_576,
                total_bytes: Some(48_210_432),
            },
            ProgressEvent::Completed { version: v("1.1.0") },
        ];
        let lines = lines_for(&events);
        assert_eq!(lines.len(), events.len());
        for line in &lines {
            assert!(line.get("event").and_then(|e| e.as_str()).is_some(), "{line}");
            assert_eq!(line["v"], STREAM_SCHEMA);
        }
    }

    #[test]
    fn a_message_is_always_present_and_matches_the_shared_wording() {
        // So a status line needs no string building, and every consumer says
        // the same thing for the same state.
        let event = ProgressEvent::Verifying { version: v("1.1.0") };
        let lines = lines_for(std::slice::from_ref(&event));
        assert_eq!(lines[0]["message"], event.message());
    }

    #[test]
    fn variant_fields_survive_with_their_numbers_intact() {
        // A progress bar is driven by these two numbers; losing either, or
        // rounding them through a float, makes the bar wrong.
        let lines = lines_for(&[ProgressEvent::DownloadProgress {
            downloaded: 1_048_576,
            total_bytes: Some(48_210_432),
        }]);
        assert_eq!(lines[0]["event"], "downloadProgress");
        assert_eq!(lines[0]["downloaded"], 1_048_576u64);
        assert_eq!(lines[0]["totalBytes"], 48_210_432u64);
    }

    #[test]
    fn an_unknown_total_is_null_rather_than_zero() {
        // A consumer must be able to tell "size unknown, show an indeterminate
        // spinner" from "size is zero", which would render a finished bar.
        let lines =
            lines_for(&[ProgressEvent::DownloadStarted { version: v("1.1.0"), total_bytes: None }]);
        // The key must be present *and* null. Asserting only that it reads as
        // null would pass if the field were missing entirely, or misnamed.
        assert!(
            lines[0].as_object().unwrap().contains_key("totalBytes"),
            "totalBytes is absent from {}",
            lines[0]
        );
        assert!(lines[0]["totalBytes"].is_null(), "got {}", lines[0]);
    }

    #[test]
    fn a_closed_pipe_never_panics() {
        // The consumer may quit at any moment. Reporting runs inside the
        // download, where unwinding would skip the cleanup of the partial file.
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
        }
        let reporter = JsonProgress::new(Closed);
        reporter.report(&ProgressEvent::Completed { version: v("1.1.0") });
    }
}
