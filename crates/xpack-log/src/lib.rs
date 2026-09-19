//! Logging setup shared by every xPack binary.
//!
//! Libraries emit through plain `tracing` macros and know nothing about how
//! logs are rendered or where they go. Only binaries call [`init`], exactly
//! once, at startup.
//!
//! # Why file logging matters here more than usual
//!
//! When the launcher rolls back a broken update there is no terminal and
//! nobody watching. The file in `state/logs/` is the only record of what
//! happened and why. That is the whole reason this crate exists: console
//! output is for the person running a command, and the file is for the failure
//! nobody saw.
//!
//! # Using it
//!
//! ## From a library
//!
//! Nothing. Emit through plain `tracing` macros and let the binary decide
//! where records go. A library that configures logging takes that decision
//! away from every program that links it.
//!
//! ```
//! # fn example(version: &str, files: usize) {
//! tracing::info!(%version, files, "version installed");
//! tracing::warn!(reason = "checksum mismatch", "download rejected");
//! # }
//! ```
//!
//! Prefer structured fields over interpolation. `version = "1.2.0"` can be
//! filtered and queried; `"installed 1.2.0"` can only be grepped.
//!
//! ## From a binary
//!
//! Call [`init`] exactly once, as early as the destination is known:
//!
//! ```no_run
//! use xpack_log::{Config, Console, Format};
//!
//! // A command that does not act on an installation: console only.
//! xpack_log::init(&Config::console_only(Console::Normal));
//! ```
//!
//! ```no_run
//! use xpack_core::InstallPaths;
//! use xpack_log::{Config, Console, Format};
//!
//! # fn example(paths: &InstallPaths) {
//! // A command that does: console plus a durable copy in the installation.
//! let outcome = xpack_log::init(&Config {
//!     console: Console::Normal,
//!     file: Some(paths),
//!     format: Format::Text,
//! });
//! if !outcome.is_enabled() {
//!     // Not an error. The operation continues without a durable log.
//! }
//! # }
//! ```
//!
//! **Decide the destination before the first call.** Installing a subscriber
//! is a one-time operation for the whole process: a later [`init`] is ignored,
//! not applied. Calling it once with console-only and again with a file gives
//! no file at all — quietly.
//!
//! ## Choosing a console level
//!
//! | Level | Use |
//! | --- | --- |
//! | [`Console::Silent`] | The process shares a terminal with something else. The launcher, whose output belongs to the application it starts. |
//! | [`Console::Quiet`] | Only warnings and errors. |
//! | [`Console::Normal`] | Default for a command someone typed. |
//! | [`Console::Verbose`] | Behind a `--verbose` flag. |
//!
//! ## What goes where
//!
//! Records go to standard error, never standard output, so a command's results
//! stay pipeable. The file, when enabled, is `state/logs/xpack.log.<date>` in
//! the installation, rotated daily and capped at [`MAX_LOG_FILES`] files.
//!
//! ## Changing levels at run time
//!
//! `XPACK_LOG` takes [`tracing_subscriber::EnvFilter`] syntax and overrides the
//! console level:
//!
//! ```text
//! XPACK_LOG=off                      silence the terminal
//! XPACK_LOG=xpack_update=debug       one component in detail
//! XPACK_LOG=xpack_install=trace,xpack_platform=debug
//! ```
//!
//! It deliberately does **not** reach the file. That variable makes a terminal
//! quieter or noisier while someone is watching; letting it silence the file
//! would destroy the forensic record written precisely for the failures nobody
//! sees. The file always records everything this project emits.
//!
//! Only this project's crates are recorded — dependency noise in an
//! installation log would bury the few lines that matter.
//!
//! ## Reading a log
//!
//! ```text
//! cat ~/.local/share/xpack/com.example.app/state/logs/xpack.log.*
//! ```
//!
//! [`Format::Json`] emits one object per record for collection and querying.
//!
//! # Never log secret material
//!
//! Logs are durable, land on disk, and get attached to bug reports. Key
//! fingerprints, paths, versions and application ids are fine. Private key
//! bytes, or anything derived from them, are not. `KeyPair`'s `Debug` prints
//! only a fingerprint for exactly this reason — do not work around it.

mod retention;

use std::path::PathBuf;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};
use xpack_core::InstallPaths;

pub use retention::MAX_LOG_FILES;

/// Environment variable that overrides every level decision.
pub const FILTER_ENV: &str = "XPACK_LOG";

/// Crates that emit log records.
///
/// Directives are built from this list rather than from a shared prefix.
/// A prefix such as `xpack=info` matches on raw string prefix, not on a `::`
/// boundary, so it happens to catch `xpack_install` today and would silently
/// start catching an unrelated `xpackfoo` tomorrow. Naming them is duller and
/// says what is actually meant.
const CRATES: [&str; 6] = [
    "xpack_cli",
    "xpack_install",
    "xpack_launcher",
    "xpack_package",
    "xpack_platform",
    "xpack_update",
];

/// How much reaches the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    /// Nothing. For a launcher sharing a terminal with the application.
    Silent,
    /// Warnings and errors only.
    Quiet,
    /// Progress and above.
    Normal,
    /// Everything, for diagnosing a problem.
    Verbose,
}

impl Console {
    fn directive(self) -> &'static str {
        match self {
            Self::Silent => "off",
            Self::Quiet => "warn",
            Self::Normal => "info",
            Self::Verbose => "debug",
        }
    }
}

/// How records are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable lines.
    Text,
    /// One JSON object per record, for collection and querying.
    Json,
}

/// What a binary wants from logging.
#[derive(Debug)]
pub struct Config<'a> {
    /// Terminal verbosity.
    pub console: Console,
    /// Installation whose `state/logs` receives a durable copy.
    ///
    /// `None` for commands with no installation to write to — building or
    /// inspecting a package happens long before one exists.
    pub file: Option<&'a InstallPaths>,
    /// Rendering for the file. The console is always human-readable.
    pub format: Format,
}

impl Config<'_> {
    /// Console-only logging at the given verbosity.
    pub fn console_only(console: Console) -> Self {
        Self { console, file: None, format: Format::Text }
    }
}

/// What happened to file logging.
///
/// Deliberately not a `Result`. A log directory that cannot be written is a
/// degraded installation, not a failed operation: refusing to install an
/// application because its log file could not be opened would be a far worse
/// outcome than the missing log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileLogging {
    /// Records are being written here.
    Enabled(PathBuf),
    /// No installation was supplied.
    Disabled,
    /// The directory could not be used; console logging still works.
    Unavailable(String),
}

impl FileLogging {
    /// Returns `true` when records are reaching a file.
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled(_))
    }
}

/// Installs the logging subscriber for this process.
///
/// Safe to call more than once: later calls are ignored rather than failing,
/// because a test binary and the code under test may both reach it.
pub fn init(config: &Config<'_>) -> FileLogging {
    // Both layers are boxed into one list rather than chained. Chaining
    // changes the subscriber's type at each step, so a boxed layer built for
    // the bare registry no longer fits once another has been added.
    let mut layers: Vec<BoxedLayer> = Vec::new();

    if config.console != Console::Silent {
        layers.push(Box::new(
            fmt::layer()
                .with_writer(std::io::stderr)
                .without_time()
                .with_target(false)
                .with_filter(filter(config.console.directive())),
        ));
    }

    let outcome = match config.file {
        None => FileLogging::Disabled,
        Some(paths) => match open_file_layer(paths, config.format) {
            Ok((layer, path)) => {
                layers.push(layer);
                FileLogging::Enabled(path)
            }
            Err(reason) => FileLogging::Unavailable(reason),
        },
    };

    let _ = tracing_subscriber::registry().with(layers).try_init();

    if let FileLogging::Unavailable(reason) = &outcome {
        tracing::warn!(reason, "file logging is unavailable; continuing with console output only");
    }
    outcome
}

/// A layer erased to fit alongside others in one list.
type BoxedLayer = Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync>;

/// Builds the file layer, or explains why it could not be built.
fn open_file_layer(
    paths: &InstallPaths,
    format: Format,
) -> std::result::Result<(BoxedLayer, PathBuf), String> {
    let dir = paths.logs_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // Old files are removed before the new one is opened, so a long-lived
    // installation cannot accumulate them without bound. Rotation alone only
    // decides when to start a new file; something has to end the old ones.
    retention::prune(&dir);

    // A blocking appender on purpose. The non-blocking writer needs a guard
    // held for the lifetime of the process, and dropping it early silently
    // truncates the log — the exact failure this crate exists to prevent, in
    // the exact situation where nobody is watching. At this volume the cost of
    // writing synchronously is irrelevant.
    let appender = tracing_appender::rolling::daily(&dir, "xpack.log");

    // The file records everything this project emits, and deliberately ignores
    // the environment override. That variable exists to make a terminal
    // quieter or noisier while someone is watching; letting it reach the file
    // would mean XPACK_LOG=off silently destroys the forensic record — the one
    // written precisely for the failures nobody sees.
    let filter = default_filter("debug");

    let layer: BoxedLayer = match format {
        Format::Text => {
            Box::new(fmt::layer().with_writer(appender).with_ansi(false).with_filter(filter))
        }
        Format::Json => {
            Box::new(fmt::layer().json().with_writer(appender).with_ansi(false).with_filter(filter))
        }
    };
    Ok((layer, dir))
}

/// Builds the console filter, honouring the environment override.
fn filter(default_level: &str) -> EnvFilter {
    if let Ok(configured) = EnvFilter::try_from_env(FILTER_ENV) {
        return configured;
    }
    default_filter(default_level)
}

/// Builds a filter from this project's crates at one level, ignoring the
/// environment.
fn default_filter(level: &str) -> EnvFilter {
    let mut directives = String::new();
    for name in CRATES {
        if !directives.is_empty() {
            directives.push(',');
        }
        directives.push_str(name);
        directives.push('=');
        directives.push_str(level);
    }
    EnvFilter::new(directives)
}

/// The directives used when nothing overrides them, for tests and diagnosis.
pub fn default_directives(console: Console) -> String {
    CRATES.map(|name| format!("{name}={}", console.directive())).join(",")
}
