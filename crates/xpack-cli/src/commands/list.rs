//! Listing installed versions.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args as ClapArgs;
use serde::Serialize;
use xpack_core::Result;
use xpack_install::Installer;

use super::Context;

/// Arguments for `xpack list`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Application to list.
    #[arg(value_name = "APPLICATION_ID")]
    pub(crate) application: String,

    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,

    /// Print the hook record instead: which hook points ran, for which
    /// version, with which script, when, for how long, and how they ended.
    #[arg(long)]
    hooks: bool,
}

/// One row of the listing.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    version: String,
    status: String,
    active: bool,
    /// Whether the files are actually present, which state alone cannot say.
    present: bool,
}

/// What `xpack list --json` prints.
///
/// An object rather than the bare array of versions it used to be, because
/// the executables an installation holds are named after the application it
/// serves and there is otherwise no way to find out what they are called. A
/// caller that reconstructed those names would be a second implementation of
/// a rule that has to stay in one place, and would be wrong for every
/// installation made before the rule existed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Listing<'a> {
    application: &'a str,
    root: &'a Path,
    /// The executable that starts the application.
    ///
    /// On Windows this is the windowed build, which is what a shortcut points
    /// at; elsewhere there is only one launcher and this is it.
    ///
    /// Null when nothing is installed at that path. Reporting a path that is
    /// not there would hand a caller something to run that does not exist.
    launcher: Option<PathBuf>,
    /// The console build, which differs from `launcher` only on Windows.
    console_launcher: Option<PathBuf>,
    updater: Option<PathBuf>,
    uninstaller: Option<PathBuf>,
    versions: Vec<Entry>,
}

/// A path, reported only when something is actually there.
fn if_present(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

/// Runs `xpack list`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    let lock = context.lock(&args.application)?;
    if args.hooks {
        return list_hooks(&lock.paths().hook_record_file(), args.json);
    }
    let installer = Installer::new(&lock);
    let state = lock.load_or_new_state(&args.application)?;
    let active = state.current_version.clone();

    let entries: Vec<Entry> = installer
        .installed()?
        .into_iter()
        .map(|(version, status)| Entry {
            active: active.as_ref() == Some(&version),
            present: installer.is_usable(&version),
            version: version.to_string(),
            status: format!("{status:?}").to_lowercase(),
        })
        .collect();

    if args.json {
        let paths = lock.paths();
        let names = state.binary_names();
        let listing = Listing {
            application: &args.application,
            root: paths.root(),
            launcher: if_present(paths.shortcut_target_named(&names)),
            console_launcher: if_present(paths.launcher_file_named(&names)),
            updater: if_present(paths.updater_file_named(&names)),
            uninstaller: if_present(paths.uninstaller_file_named(&names)),
            versions: entries,
        };
        return crate::output::json(&listing).map(|()| ExitCode::SUCCESS);
    }

    if entries.is_empty() {
        xpack_core::outln!("no versions installed");
        return super::success();
    }

    for entry in &entries {
        let marker = if entry.active { "*" } else { " " };
        let missing = if entry.present { "" } else { "   (files missing)" };
        xpack_core::outln!("{marker} {:<16} {}{missing}", entry.version, entry.status);
    }

    if !state.update.is_idle() {
        xpack_core::errln!();
        xpack_core::errln!("note: {}", state.update.describe());
    }
    super::success()
}

/// Prints the hook record, a line per event, as written: read as it is, a
/// line that cannot be read included, never repaired or summarised.
fn list_hooks(record: &Path, json: bool) -> Result<ExitCode> {
    let text = match std::fs::read_to_string(record) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(xpack_core::Error::io(record, error)),
    };
    let lines: Vec<std::result::Result<xpack_core::hooks::HookRecordLine, &str>> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|_| line))
        .collect();
    if json {
        let values: Vec<serde_json::Value> = lines
            .iter()
            .map(|line| match line {
                Ok(line) => serde_json::to_value(line).unwrap_or(serde_json::Value::Null),
                Err(raw) => serde_json::json!({ "unreadable": raw }),
            })
            .collect();
        return crate::output::json(&values).map(|()| ExitCode::SUCCESS);
    }
    if lines.is_empty() {
        xpack_core::outln!("no hook has run");
        return super::success();
    }
    for line in &lines {
        match line {
            Ok(line) => {
                let took = line.seconds.map(|s| format!(" ({s} s)")).unwrap_or_default();
                let event = format!("{:?}", line.event).to_lowercase();
                xpack_core::outln!(
                    "{:<12} {:<20} {:<10} {}  {}{took}",
                    line.version,
                    line.point.to_string(),
                    event,
                    line.script,
                    utc(line.at)
                );
            }
            Err(_) => xpack_core::outln!("(a line that cannot be read)"),
        }
    }
    super::success()
}

/// `seconds` since the Unix epoch, as a UTC date and time a person reads.
fn utc(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let rest = seconds % 86_400;
    // Days to a civil date, after Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::utc;

    #[test]
    fn epoch_seconds_read_as_a_date() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(1_790_972_643), "2026-10-02 20:24:03 UTC");
        assert_eq!(utc(4_107_542_399), "2100-02-28 23:59:59 UTC");
        // Each term of the century and era corrections, past the dates that
        // need none of them.
        assert_eq!(utc(946_684_799), "1999-12-31 23:59:59 UTC");
        assert_eq!(utc(4_107_542_400), "2100-03-01 00:00:00 UTC");
        assert_eq!(utc(7_263_259_200), "2200-03-01 12:00:00 UTC");
        assert_eq!(utc(13_574_649_599), "2400-02-29 23:59:59 UTC");
        assert_eq!(utc(13_574_649_600), "2400-03-01 00:00:00 UTC");
    }
}
