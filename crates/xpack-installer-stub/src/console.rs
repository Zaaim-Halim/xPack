//! The installer without a window.
//!
//! What a script runs, and what every installer falls back to when no window
//! can be shown. Output goes to the console and the result is the exit code.

use std::process::ExitCode;

use clap::Parser;
use xpack_install::Existing;
use xpack_installer::{Request, RootSource, exit};
use xpack_installer_ui::AllUsers;

// A plain comment, not a doc comment: clap turns doc comments into the help
// a user reads. The bool count trips a lint meant for domain types, where
// several flags usually mean a missing enum; these are command-line switches,
// each independently settable, and clap requires exactly this shape.
/// Installs the application this installer carries.
///
/// Run with no options, the macOS bundle and the windowed Windows build open
/// the installation wizard. `--silent` installs without a window, and
/// `--dry-run` says what would be installed without installing it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Parser)]
#[command(name = "xpack-installer", version, about = "Installs an xPack application")]
pub(crate) struct Args {
    /// Install under this directory instead of the per-user default.
    ///
    /// The directory holding every xPack application for this user; the
    /// application's own id is appended to it.
    #[arg(long, value_name = "DIR", env = xpack_core::paths::INSTALL_ROOT_ENV)]
    pub(crate) root: Option<std::path::PathBuf>,

    /// Install without showing a window.
    ///
    /// The installers that open a wizard — the macOS bundle and the windowed
    /// Windows build — do so only without this. Every build accepts it, so one
    /// command line works with all of them.
    #[arg(long)]
    pub(crate) silent: bool,

    /// Do not add the desktop entry the application asks for.
    ///
    /// Only on a first installation: an existing one keeps the updater it was
    /// installed with, which would put the entry back, so the request is
    /// refused there rather than silently undone later.
    #[arg(long)]
    pub(crate) no_shortcut: bool,

    /// Do not put a shortcut on the desktop beside the desktop entry.
    ///
    /// One is added on a first installation that adds the entry, unless this
    /// says not to. An existing installation never gets one.
    #[arg(long)]
    pub(crate) no_desktop_shortcut: bool,

    /// Do not add the command the application asks for.
    ///
    /// The command is what a terminal starts the application by. On the same
    /// terms as `--no-shortcut`: only on a first installation.
    #[arg(long)]
    pub(crate) no_path: bool,

    /// Install for everyone on this computer, where the installer offers it.
    ///
    /// Needs administrator rights: run it with `sudo` on macOS and Linux, or
    /// from an elevated terminal on Windows. It goes where other programs do,
    /// named after the application, and `--root` does not apply.
    #[arg(long, conflicts_with_all = ["only_me", "root"])]
    pub(crate) all_users: bool,

    /// Install for the person running this only, where the installer offers
    /// the choice.
    #[arg(long)]
    pub(crate) only_me: bool,

    /// Describe what would be installed, without installing it.
    ///
    /// Exits with the code the installation itself would, so a script can ask
    /// first.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Also write a log of this run to this file, appending.
    ///
    /// A run that opens a window always keeps one, in the user's temporary
    /// directory unless this names another: with no console, it is the only
    /// account of a failure. The failure page says where it is.
    #[arg(long, value_name = "FILE")]
    pub(crate) log: Option<std::path::PathBuf>,

    /// Print more detail about what is happening.
    #[arg(short, long)]
    pub(crate) verbose: bool,
}

/// The installer without a window: what a script sees.
pub(crate) fn run(args: &Args) -> xpack_core::Result<ExitCode> {
    // Verified before anything is printed, so every line below describes a
    // package whose signature has been checked.
    let payload = crate::load()?;
    let application = &payload.manifest().application;
    xpack_core::outln!("{} {}", application.name, application.version);

    let ui = payload.ui();
    if scope(ui.all_users, ui.all_users_default, args)? == xpack_core::InstallScope::Machine {
        return run_for_everyone(args, &payload);
    }

    let resolved = payload.resolve_root(args.root.as_deref())?;
    let root = resolved.root;
    match resolved.source {
        RootSource::Found => xpack_core::outln!("already installed under {}", root.display()),
        RootSource::Given => {
            // Allowed, because it was asked for; said, because the menu entry
            // and the uninstall entry will point here afterwards, not there.
            if let Some(other) = payload.recorded_root().filter(|other| *other != root) {
                xpack_core::errln!(
                    "note: {} is also installed under {}; after this, its menu entry points here",
                    application.name,
                    other.display()
                );
            }
        }
        RootSource::Default => {}
    }
    let existing = payload.inspect(&root)?;
    if let Some(line) = describe(&existing) {
        xpack_core::outln!("{line}");
    }

    if args.dry_run {
        xpack_core::outln!("would install into {}", payload.paths(&root)?.root().display());
        xpack_core::outln!("signed by       {}", short_key(&payload.plan().signing_key));
        if payload.manifest().desktop.shortcut
            && !args.no_shortcut
            && !args.no_desktop_shortcut
            && existing.is_first_install()
        {
            xpack_core::outln!("would add       a shortcut on the desktop");
        }
        if let Some(command) = &payload.manifest().command
            && !args.no_path
        {
            xpack_core::outln!("would add       the command {}", command.name);
        }
        return Ok(ExitCode::from(dry_run_code(&existing)));
    }

    let request = request(args, root, xpack_core::InstallScope::User);
    let outcome = payload.install_into(&request, &xpack_core::NoProgress)?;
    report(&outcome, payload.manifest());
    Ok(ExitCode::from(exit::INSTALLED))
}

/// The request the options describe.
fn request(args: &Args, root: std::path::PathBuf, scope: xpack_core::InstallScope) -> Request {
    Request {
        root,
        desktop_entry: if args.no_shortcut { Some(false) } else { None },
        desktop_shortcut: !args.no_desktop_shortcut,
        command: if args.no_path { Some(false) } else { None },
        scope,
    }
}

/// Who to install for: what the publisher allows, then what was asked.
fn scope(
    allowed: AllUsers,
    everyone_by_default: bool,
    args: &Args,
) -> xpack_core::Result<xpack_core::InstallScope> {
    use xpack_core::InstallScope::{Machine, User};
    match allowed {
        AllUsers::Never if args.all_users => Err(xpack_core::Error::invalid(
            "--all-users",
            "this application is installed for one person at a time",
        )),
        AllUsers::Always if args.only_me => Err(xpack_core::Error::invalid(
            "--only-me",
            "this application is installed for everyone on the computer",
        )),
        AllUsers::Never => Ok(User),
        AllUsers::Always => Ok(Machine),
        AllUsers::Offer if args.all_users => Ok(Machine),
        AllUsers::Offer if args.only_me => Ok(User),
        AllUsers::Offer => Ok(if everyone_by_default { Machine } else { User }),
    }
}

/// Installs for everyone on the computer, which needs administrator rights.
fn run_for_everyone(
    args: &Args,
    payload: &xpack_installer::VerifiedPayload,
) -> xpack_core::Result<ExitCode> {
    let application = &payload.manifest().application;
    let directory =
        xpack_install::integration::machine_application_dir(&application.id, &application.name)?;
    if args.dry_run {
        xpack_core::outln!("would install into {} for everyone", directory.display());
        xpack_core::outln!("signed by       {}", short_key(&payload.plan().signing_key));
        return Ok(ExitCode::from(exit::INSTALLED));
    }
    if !xpack_platform::is_elevated() {
        let how = if cfg!(windows) {
            "run it again from a terminal opened with \"Run as administrator\""
        } else {
            "run it again with sudo"
        };
        return Err(xpack_core::Error::invalid(
            "installation",
            format!("installing for everyone needs administrator rights: {how}"),
        ));
    }
    let request = request(args, directory, xpack_core::InstallScope::Machine);
    let outcome = payload.install_into(&request, &xpack_core::NoProgress)?;
    report(&outcome, payload.manifest());
    Ok(ExitCode::from(exit::INSTALLED))
}

/// Says what was installed and how to start it.
fn report(outcome: &xpack_installer::Outcome, manifest: &xpack_core::Manifest) {
    xpack_core::outln!();
    xpack_core::outln!("Installed into {}", outcome.root.display());
    for outcome in [&outcome.desktop, &outcome.desktop_shortcut] {
        if let xpack_install::DesktopOutcome::Done(entries) = outcome {
            for entry in entries {
                xpack_core::outln!("Added         {}", entry.display());
            }
        }
    }
    xpack_core::outln!();
    xpack_core::outln!("Run it with:   {}", outcome.launcher.display());
    report_command(outcome, manifest);
}

/// Says how to start the application from a terminal, or why that was not set
/// up, when the package asked for a command.
fn report_command(outcome: &xpack_installer::Outcome, manifest: &xpack_core::Manifest) {
    if let Some((name, others)) = outcome.command_names.split_first() {
        xpack_core::outln!("Or type:       {name}   (in a new terminal window)");
        if !others.is_empty() {
            xpack_core::outln!("Also:          {}", others.join(", "));
        }
        if let Some(dir) = &outcome.command_off_path {
            xpack_core::outln!();
            xpack_core::outln!(
                "{} is not on your PATH, so a terminal will not find it yet.",
                dir.display()
            );
            xpack_core::outln!("Add this line to your shell's profile, then open a new terminal:");
            xpack_core::outln!("  export PATH=\"{}:$PATH\"", dir.display());
        }
    } else if let (Some(command), xpack_install::DesktopOutcome::Failed(reason)) =
        (&manifest.command, &outcome.command)
    {
        // The installation is fine; only the shortcut to it by name is not.
        xpack_core::errln!("note: the command {} was not added: {reason}", command.name);
    }
}

/// A line saying what the installation will be, where that is worth saying.
///
/// A refusal is described here only as a forecast; the refusal itself, and
/// its wording, come from the installer when it is asked to go ahead.
fn describe(existing: &Existing) -> Option<String> {
    match existing {
        Existing::Nothing | Existing::Inactive => None,
        Existing::Older(version) => Some(format!("upgrading from {version}")),
        Existing::Damaged => Some("repairing the installed copy of this version".to_string()),
        Existing::Installed => Some("this version is already installed".to_string()),
        Existing::Newer(version) => Some(format!("a newer version ({version}) is installed")),
        Existing::Busy => Some("another xPack operation is using this installation".to_string()),
        Existing::Unreadable(reason) => Some(format!("the installation cannot be read: {reason}")),
    }
}

/// The code a dry run exits with: the one installing would.
fn dry_run_code(existing: &Existing) -> u8 {
    match existing {
        _ if existing.allows_install() => exit::INSTALLED,
        Existing::Busy => exit::BUSY,
        _ => exit::FAILED,
    }
}

/// A short, readable form of the pinned key, for the person installing.
///
/// The full 64 characters are unreadable and nobody compares them by eye. The
/// leading group is enough to tell two publishers apart when someone has been
/// told what to expect.
fn short_key(hex: &str) -> String {
    hex.chars().take(16).collect::<String>()
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use xpack_core::InstallScope::{Machine, User};

    use super::{AllUsers, Args, scope};

    fn args(flags: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("installer").chain(flags.iter().copied())).unwrap()
    }

    #[test]
    fn who_it_installs_for_follows_the_publisher_then_the_person() {
        assert_eq!(scope(AllUsers::Never, false, &args(&[])).unwrap(), User);
        assert_eq!(scope(AllUsers::Always, false, &args(&[])).unwrap(), Machine);
        assert_eq!(scope(AllUsers::Offer, false, &args(&[])).unwrap(), User);
        assert_eq!(scope(AllUsers::Offer, true, &args(&[])).unwrap(), Machine);
        assert_eq!(scope(AllUsers::Offer, true, &args(&["--only-me"])).unwrap(), User);
        assert_eq!(scope(AllUsers::Offer, false, &args(&["--all-users"])).unwrap(), Machine);
    }

    #[test]
    fn asking_for_what_the_publisher_does_not_allow_is_refused() {
        assert!(scope(AllUsers::Never, false, &args(&["--all-users"])).is_err());
        assert!(scope(AllUsers::Always, false, &args(&["--only-me"])).is_err());
    }

    #[test]
    fn everyone_and_a_chosen_folder_do_not_go_together() {
        let both = ["installer", "--all-users", "--root", "/somewhere"];
        assert!(Args::try_parse_from(both).is_err());
        assert!(Args::try_parse_from(["installer", "--all-users", "--only-me"]).is_err());
    }

    #[test]
    fn the_help_a_user_reads_is_written_for_them() {
        // Doc comments become `--help`. A note meant for whoever maintains the
        // code once reached every user who asked the installer for help.
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("Installs the application this installer carries"), "{help}");
        for internal in ["lint", "clippy", "clap", "command line."] {
            assert!(!help.contains(internal), "--help mentions {internal:?}:\n{help}");
        }
    }
}
