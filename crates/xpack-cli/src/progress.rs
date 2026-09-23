//! Showing an update's progress in a terminal.
//!
//! Rendering lives here and nowhere else. The engine emits events and has no
//! opinion about bars, colours or terminals; this decides how they look.

use std::io::IsTerminal;
use std::sync::Mutex;

use indicatif::{ProgressBar, ProgressStyle};
use xpack_core::progress::{ProgressEvent, ProgressReporter};

/// Draws progress on the terminal, or prints plain lines when there is none.
pub(crate) struct TerminalProgress {
    bar: Mutex<Option<ProgressBar>>,
    /// Whether anything is attached that can interpret a redrawn line.
    interactive: bool,
}

impl TerminalProgress {
    /// Builds a reporter for the current terminal.
    ///
    /// Progress goes to standard error, never standard output. It is not a
    /// result, and writing it to standard output would corrupt anything being
    /// piped — `xpack list --json` most obviously.
    pub(crate) fn new() -> Self {
        Self { bar: Mutex::new(None), interactive: std::io::stderr().is_terminal() }
    }

    /// Builds a reporter that prints plain lines, whatever is attached.
    ///
    /// For a caller that wants the words without the redrawing — a log, a CI
    /// job, a terminal being captured by something else.
    pub(crate) fn plain() -> Self {
        Self { bar: Mutex::new(None), interactive: false }
    }

    /// Clears any bar still on screen.
    pub(crate) fn finish(&self) {
        if let Ok(mut slot) = self.bar.lock()
            && let Some(bar) = slot.take()
        {
            bar.finish_and_clear();
        }
    }

    fn set_bar(&self, bar: Option<ProgressBar>) {
        if let Ok(mut slot) = self.bar.lock() {
            if let Some(previous) = slot.take() {
                previous.finish_and_clear();
            }
            *slot = bar;
        }
    }

    /// Prints a line, stepping around a bar that is currently drawn.
    fn line(&self, text: &str) {
        if let Ok(slot) = self.bar.lock()
            && let Some(bar) = slot.as_ref()
        {
            bar.suspend(|| xpack_core::errln!("{text}"));
            return;
        }
        xpack_core::errln!("{text}");
    }
}

/// A bar with a known total.
fn determinate(total: u64, label: &str) -> ProgressBar {
    let bar = ProgressBar::new(total);
    bar.set_style(
        ProgressStyle::with_template(
            "  {msg:<12} {bar:40.cyan/blue} {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>11} {eta:>6}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        // Half-height blocks: a bar that sits on the baseline rather than
        // filling the whole cell. Heavier and easier to judge than a
        // line-drawing rule, without the full-block version's tendency to
        // dominate the line it shares with the byte counts.
        .progress_chars("▄▄▁"),
    );
    bar.set_message(label.to_string());
    bar
}

/// A spinner, for when the total is unknown or untrustworthy.
fn indeterminate(label: &str) -> ProgressBar {
    let bar = ProgressBar::new_spinner();
    bar.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} {msg:<12} {bytes:>10} {bytes_per_sec:>11}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    bar.set_message(label.to_string());
    bar.enable_steady_tick(std::time::Duration::from_millis(120));
    bar
}

impl ProgressReporter for TerminalProgress {
    fn report(&self, event: &ProgressEvent) {
        match event {
            ProgressEvent::DownloadStarted { total_bytes, .. } => {
                if !self.interactive {
                    self.line(&format!("  {}", event.message()));
                    return;
                }
                // A total the server declared is not trustworthy, so an absent
                // one becomes a spinner rather than a bar against zero.
                self.set_bar(Some(match total_bytes {
                    Some(total) => determinate(*total, "Downloading"),
                    None => indeterminate("Downloading"),
                }));
            }

            ProgressEvent::DownloadProgress { downloaded, .. } => {
                if let Ok(slot) = self.bar.lock()
                    && let Some(bar) = slot.as_ref()
                {
                    bar.set_position(*downloaded);
                }
            }

            ProgressEvent::DownloadCompleted { .. } => {
                self.set_bar(None);
                self.line(&format!("  {}", event.message()));
            }

            ProgressEvent::ExtractionProgress { files_completed, files_total, .. } => {
                if !self.interactive {
                    return;
                }
                let should_start = self.bar.lock().is_ok_and(|slot| {
                    slot.as_ref().is_none_or(|b| b.length() != Some(*files_total as u64))
                });
                if should_start {
                    let bar = ProgressBar::new(*files_total as u64);
                    bar.set_style(
                        ProgressStyle::with_template(
                            "  {msg:<12} {bar:40.green/blue} {pos:>6}/{len:<6} files",
                        )
                        .unwrap_or_else(|_| ProgressStyle::default_bar())
                        .progress_chars("▄▄▁"),
                    );
                    bar.set_message("Installing");
                    self.set_bar(Some(bar));
                }
                if let Ok(slot) = self.bar.lock()
                    && let Some(bar) = slot.as_ref()
                {
                    bar.set_position(*files_completed as u64);
                }
            }

            // Everything else is a stage change, which reads better as a line.
            other => {
                if matches!(other, ProgressEvent::Installing { .. }) && self.interactive {
                    // The extraction bar says this already.
                    return;
                }
                self.set_bar(None);
                self.line(&format!("  {}", other.message()));
            }
        }
    }
}

/// Where progress goes, and in what shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub(crate) enum ProgressMode {
    /// A bar when a terminal is attached, plain lines otherwise.
    #[default]
    Auto,
    /// One JSON object per line on stdout, for a program to read.
    Json,
    /// Plain lines, never a bar.
    Plain,
    /// Nothing at all.
    None,
}

/// A reporter and the terminal bar it may own.
///
/// The bar has to be cleared before anything else is printed, or a failure
/// message interleaves with a half-drawn line. Holding both together means a
/// caller cannot forget which one it has.
pub(crate) enum Progress {
    Terminal(TerminalProgress),
    Json(xpack_core::JsonProgress<std::io::Stdout>),
    Silent(xpack_core::NoProgress),
}

impl Progress {
    /// Builds the reporter a mode asks for.
    pub(crate) fn new(mode: ProgressMode) -> Self {
        match mode {
            ProgressMode::Auto => Self::Terminal(TerminalProgress::new()),
            ProgressMode::Plain => Self::Terminal(TerminalProgress::plain()),
            ProgressMode::Json => Self::Json(xpack_core::JsonProgress::to_stdout()),
            ProgressMode::None => Self::Silent(xpack_core::NoProgress),
        }
    }

    /// The reporter to hand to the engine.
    pub(crate) fn reporter(&self) -> &dyn xpack_core::ProgressReporter {
        match self {
            Self::Terminal(p) => p,
            Self::Json(p) => p,
            Self::Silent(p) => p,
        }
    }

    /// Clears any bar before other output is written.
    pub(crate) fn finish(&self) {
        if let Self::Terminal(p) = self {
            p.finish();
        }
    }

    /// Whether the command should also print its own human-readable result.
    ///
    /// In JSON mode it must not: the stream is the result, and a stray line of
    /// prose in the middle of it breaks the parser reading it.
    pub(crate) fn wants_human_output(&self) -> bool {
        !matches!(self, Self::Json(_))
    }
}
