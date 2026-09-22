//! The installation's progress, as the Installing page shows it.

use xpack_core::ProgressEvent;

use super::text::{Key, Texts};

/// What the engine has reported so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    status: String,
    counts: Option<Counts>,
}

/// Files and bytes written, against what the signed manifest declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Files written so far.
    pub files_done: usize,
    /// Files the manifest declares.
    pub files_total: usize,
    /// Bytes written so far.
    pub bytes_done: u64,
    /// Bytes the manifest declares.
    pub bytes_total: u64,
}

impl Progress {
    /// Takes in one event from the engine.
    ///
    /// The status line uses the engine's own wording for every event, the
    /// same words a terminal shows, so the two never describe one state
    /// differently.
    pub fn observe(&mut self, event: &ProgressEvent) {
        self.status = event.message();
        if let ProgressEvent::ExtractionProgress {
            files_completed,
            files_total,
            bytes_completed,
            bytes_total,
        } = event
        {
            self.counts = Some(Counts {
                files_done: *files_completed,
                files_total: *files_total,
                bytes_done: *bytes_completed,
                bytes_total: *bytes_total,
            });
        }
    }

    /// The line above the bar.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// The counts, once there are any.
    pub fn counts(&self) -> Option<Counts> {
        self.counts
    }

    /// How far along, in thousandths, or `None` for an indeterminate bar.
    ///
    /// Indeterminate until the first count arrives, rather than sitting at
    /// zero and looking stuck.
    pub fn permille(&self) -> Option<u16> {
        self.counts.map(Counts::permille)
    }

    /// The line under the bar, or `None` before there are counts.
    pub fn counts_line(&self, texts: &Texts) -> Option<String> {
        let counts = self.counts?;
        Some(texts.line_with(
            Key::InstallingCounts,
            &[
                ("files_done", &counts.files_done.to_string()),
                ("files_total", &counts.files_total.to_string()),
                ("mb_done", &megabytes(counts.bytes_done)),
                ("mb_total", &megabytes(counts.bytes_total)),
            ],
        ))
    }
}

impl Counts {
    /// Thousandths done, by bytes, falling back to files when no bytes are
    /// declared, and never past the end.
    fn permille(self) -> u16 {
        let (done, total) = if self.bytes_total > 0 {
            (self.bytes_done, self.bytes_total)
        } else {
            (self.files_done as u64, self.files_total as u64)
        };
        if total == 0 {
            return 1000;
        }
        let permille = u128::from(done.min(total)) * 1000 / u128::from(total);
        u16::try_from(permille).unwrap_or(1000)
    }
}

/// Bytes as megabytes to one decimal place, counted in 1 048 576-byte units
/// as the systems' own file managers count them.
fn megabytes(bytes: u64) -> String {
    let tenths = u128::from(bytes) * 10 / 1_048_576;
    format!("{}.{}", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use xpack_core::Version;

    use super::*;
    use crate::model::text::{Facts, Flavour};

    fn texts() -> Texts {
        let facts = Facts {
            name: "App".into(),
            version: Version::parse("2.0.0").unwrap(),
            publisher: None,
            description: None,
        };
        Texts::new(Flavour::Mac, facts, BTreeMap::new())
    }

    fn extraction(files: usize, bytes: u64) -> ProgressEvent {
        ProgressEvent::ExtractionProgress {
            files_completed: files,
            files_total: 40,
            bytes_completed: bytes,
            bytes_total: 20 * 1_048_576,
        }
    }

    #[test]
    fn the_bar_is_indeterminate_until_counts_arrive() {
        let mut progress = Progress::default();
        progress.observe(&ProgressEvent::Installing { version: Version::parse("2.0.0").unwrap() });
        assert_eq!(progress.permille(), None);
        assert_eq!(progress.status(), "Installing 2.0.0");
        assert_eq!(progress.counts_line(&texts()), None);
    }

    #[test]
    fn the_bar_follows_the_bytes_written() {
        let mut progress = Progress::default();
        progress.observe(&extraction(10, 5 * 1_048_576));
        assert_eq!(progress.permille(), Some(250));
        assert_eq!(
            progress.counts_line(&texts()).as_deref(),
            Some("10 of 40 files · 5.0 of 20.0 MB")
        );
    }

    #[test]
    fn later_events_keep_the_last_counts() {
        let mut progress = Progress::default();
        progress.observe(&extraction(40, 20 * 1_048_576));
        progress.observe(&ProgressEvent::Activating { version: Version::parse("2.0.0").unwrap() });
        assert_eq!(progress.permille(), Some(1000));
        assert_eq!(progress.status(), "Activating 2.0.0");
    }

    #[test]
    fn a_count_past_its_total_never_draws_past_the_end() {
        let counts = Counts { files_done: 9, files_total: 3, bytes_done: 900, bytes_total: 300 };
        assert_eq!(counts.permille(), 1000);
    }

    #[test]
    fn empty_totals_do_not_divide_by_zero() {
        let files = Counts { files_done: 1, files_total: 4, bytes_done: 0, bytes_total: 0 };
        assert_eq!(files.permille(), 250);
        let nothing = Counts { files_done: 0, files_total: 0, bytes_done: 0, bytes_total: 0 };
        assert_eq!(nothing.permille(), 1000);
    }

    #[test]
    fn megabytes_are_rounded_down_to_a_tenth() {
        assert_eq!(megabytes(0), "0.0");
        assert_eq!(megabytes(1_048_575), "0.9");
        assert_eq!(megabytes(3 * 1_048_576 + 524_288), "3.5");
        assert_eq!(megabytes(u64::MAX), "17592186044415.9");
    }
}
