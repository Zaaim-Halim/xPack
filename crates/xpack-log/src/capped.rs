//! Log files that stop growing at a size limit.
//!
//! Rotating by day bounds how many files there are, not how big one gets. A
//! program that logs in a loop could fill the user's disk within a single day,
//! which is a worse failure than the one it is logging. So each file stops
//! accepting lines at [`MAX_LOG_FILE_BYTES`], says so once, and the next day's
//! file starts afresh.
//!
//! # The size is read from the file, not counted
//!
//! Every xPack process of an installation — the launcher, the updater, each
//! command — appends to the same day's file. A count kept inside one process
//! would not see what the others wrote, so the limit is checked against the
//! file's actual length before each line.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::fmt::MakeWriter;

use crate::Format;

/// The most one log file grows to, give or take the line that crosses it.
///
/// Sixteen MiB is weeks of ordinary logging for one installation, and with
/// [`crate::MAX_LOG_FILES`] daily files it bounds the whole log directory at
/// about 80 MiB.
pub const MAX_LOG_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// What the file is called.
enum Naming {
    /// `<prefix>.<YYYY-MM-DD>`, by the UTC date: one file a day.
    Daily { prefix: String, today: Box<dyn Fn() -> u64 + Send + Sync> },
    /// One file, appended to every time.
    Fixed(PathBuf),
}

/// A log destination that stops growing at a limit.
pub(crate) struct CappedLog {
    dir: PathBuf,
    naming: Naming,
    limit: u64,
    format: Format,
    open: Mutex<Option<(PathBuf, File)>>,
}

impl CappedLog {
    /// One file a day in `dir`, named `<prefix>.<YYYY-MM-DD>`.
    pub(crate) fn daily(dir: PathBuf, prefix: &str, format: Format) -> Self {
        Self::daily_with(dir, prefix, format, MAX_LOG_FILE_BYTES, Box::new(utc_day_now))
    }

    /// One file, appended to by every run.
    pub(crate) fn fixed(path: PathBuf, format: Format) -> Self {
        let dir = path.parent().map(PathBuf::from).unwrap_or_default();
        Self::with(dir, Naming::Fixed(path), format, MAX_LOG_FILE_BYTES)
    }

    /// As [`Self::daily`], with the limit and the day chosen by the caller.
    fn daily_with(
        dir: PathBuf,
        prefix: &str,
        format: Format,
        limit: u64,
        today: Box<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self::with(dir, Naming::Daily { prefix: prefix.to_string(), today }, format, limit)
    }

    fn with(dir: PathBuf, naming: Naming, format: Format, limit: u64) -> Self {
        Self { dir, naming, limit, format, open: Mutex::new(None) }
    }

    fn current_path(&self) -> PathBuf {
        match &self.naming {
            Naming::Daily { prefix, today } => {
                let (year, month, day) = civil_from_days(today());
                self.dir.join(format!("{prefix}.{year:04}-{month:02}-{day:02}"))
            }
            Naming::Fixed(path) => path.clone(),
        }
    }

    /// Appends one formatted record, unless the file is already full.
    fn append(&self, record: &[u8]) -> io::Result<()> {
        let path = self.current_path();
        let mut open = self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if open.as_ref().is_none_or(|(current, _)| *current != path) {
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            // A new day's file: a process running for days would otherwise
            // add one file a day and never remove any.
            if matches!(self.naming, Naming::Daily { .. }) {
                crate::retention::prune(&self.dir);
            }
            *open = Some((path, file));
        }
        let Some((_, file)) = open.as_mut() else {
            return Ok(());
        };

        // The length on disk, which includes what other processes wrote.
        if file.metadata()?.len() >= self.limit {
            return Ok(());
        }
        file.write_all(record)?;
        if file.metadata()?.len() >= self.limit {
            file.write_all(self.full_notice().as_bytes())?;
        }
        Ok(())
    }

    /// The last line a full file gets, in the file's own format.
    fn full_notice(&self) -> String {
        let mib = self.limit / (1024 * 1024);
        let rest = match self.naming {
            Naming::Daily { .. } => "later lines today are dropped; tomorrow's file starts empty",
            Naming::Fixed(_) => "later lines are dropped",
        };
        let message = format!("this log reached its {mib} MiB limit; {rest}");
        match self.format {
            Format::Text => format!("xpack-log: {message}\n"),
            Format::Json => {
                let value = serde_json::json!({
                    "level": "WARN",
                    "target": "xpack_log",
                    "fields": { "message": message },
                });
                format!("{value}\n")
            }
        }
    }
}

/// Writes whole records to a [`CappedLog`].
pub(crate) struct CappedWriter<'a>(&'a CappedLog);

impl Write for CappedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // A dropped line is reported as written: failing here would only make
        // the subscriber complain, on the console, about a limit that exists
        // on purpose.
        self.0.append(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CappedLog {
    type Writer = CappedWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        CappedWriter(self)
    }
}

/// Days since 1970-01-01, in UTC.
fn utc_day_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() / 86_400)
}

/// The calendar date `days` after 1970-01-01, as (year, month, day).
///
/// Howard Hinnant's `civil_from_days`, for the proleptic Gregorian calendar.
/// Only dates on or after 1970 arise here, so the arithmetic stays unsigned.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn dates_are_the_calendar_dates_a_person_would_write() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        // A leap day, and the day after it.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        assert_eq!(civil_from_days(20_721), (2026, 9, 25));
        assert_eq!(civil_from_days(20_818), (2026, 12, 31));
    }

    #[test]
    fn a_daily_file_is_named_by_its_utc_date() {
        let dir = tempfile::tempdir().unwrap();
        let log = CappedLog::daily_with(
            dir.path().into(),
            "xpack.log",
            Format::Text,
            1024,
            Box::new(|| 20_721),
        );
        log.append(b"a line\n").unwrap();
        assert_eq!(read(&dir.path().join("xpack.log.2026-09-25")), "a line\n");
    }

    #[test]
    fn a_full_file_takes_no_more_lines_and_says_so_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = CappedLog::daily_with(
            dir.path().into(),
            "xpack.log",
            Format::Text,
            100,
            Box::new(|| 0),
        );
        for i in 0..1000 {
            log.append(format!("line {i:04} of a program logging in a loop\n").as_bytes()).unwrap();
        }
        let text = read(&dir.path().join("xpack.log.1970-01-01"));
        assert!(text.len() < 300, "{} bytes: {text}", text.len());
        assert_eq!(text.matches("reached its").count(), 1, "{text}");
        assert!(text.ends_with("tomorrow's file starts empty\n"), "{text}");
    }

    #[test]
    fn a_new_day_starts_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let day = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let clock = std::sync::Arc::clone(&day);
        let log = CappedLog::daily_with(
            dir.path().into(),
            "xpack.log",
            Format::Text,
            100,
            Box::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)),
        );
        for _ in 0..50 {
            log.append(b"filling the first day up\n").unwrap();
        }
        day.store(1, std::sync::atomic::Ordering::SeqCst);
        log.append(b"the next day\n").unwrap();
        assert_eq!(read(&dir.path().join("xpack.log.1970-01-02")), "the next day\n");
    }

    #[test]
    fn a_process_running_for_days_keeps_only_the_newest_files() {
        let dir = tempfile::tempdir().unwrap();
        let day = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let clock = std::sync::Arc::clone(&day);
        let log = CappedLog::daily_with(
            dir.path().into(),
            "xpack.log",
            Format::Text,
            1024,
            Box::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)),
        );
        for d in 0..20 {
            day.store(d, std::sync::atomic::Ordering::SeqCst);
            log.append(b"still running\n").unwrap();
        }
        let mut files: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files.len(), crate::MAX_LOG_FILES, "{files:?}");
        assert_eq!(files.last().unwrap(), "xpack.log.1970-01-20");
    }

    #[test]
    fn a_file_another_process_filled_takes_nothing_more() {
        // Other xPack processes append to the same file; what they wrote
        // counts against the limit.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xpack.log.1970-01-01");
        std::fs::write(&path, vec![b'x'; 200]).unwrap();
        let log = CappedLog::daily_with(
            dir.path().into(),
            "xpack.log",
            Format::Text,
            100,
            Box::new(|| 0),
        );
        log.append(b"one more\n").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 200);
    }

    #[test]
    fn a_single_named_file_is_capped_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installer.log");
        let mut log = CappedLog::fixed(path.clone(), Format::Text);
        log.limit = 100;
        for _ in 0..100 {
            log.append(b"an installer run that logs a lot\n").unwrap();
        }
        let text = read(&path);
        assert!(text.len() < 300, "{} bytes", text.len());
        assert!(text.ends_with("later lines are dropped\n"), "{text}");
    }

    #[test]
    fn a_json_log_ends_with_a_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = CappedLog::daily_with(
            dir.path().into(),
            "xpack.log",
            Format::Json,
            100,
            Box::new(|| 0),
        );
        for _ in 0..20 {
            log.append(b"{\"level\":\"INFO\"}\n").unwrap();
        }
        let text = read(&dir.path().join("xpack.log.1970-01-01"));
        for line in text.lines() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("{line}: {e}"));
        }
        assert!(text.contains("reached its"), "{text}");
    }
}
