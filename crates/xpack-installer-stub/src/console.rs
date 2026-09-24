//! The installer without a window.
//!
//! What a script runs, and what every installer falls back to when no window
//! can be shown. Output goes to the console and the result is the exit code.

use std::process::ExitCode;

use clap::Parser;
use xpack_install::Existing;
use xpack_installer::{Request, RootSource, exit};

/// The installer's command line.
///
/// The bool count trips a lint meant for domain types, where several flags
/// usually mean a missing enum. These are command-line switches: each one is
/// independently settable and clap requires exactly this shape.
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

    /// Do not add the command the application asks for.
    ///
    /// The command is what a terminal starts the application by. On the same
    /// terms as `--no-shortcut`: only on a first installation.
    #[arg(long)]
    pub(crate) no_path: bool,

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
        if let Some(command) = &payload.manifest().command
            && !args.no_path
        {
            xpack_core::outln!("would add       the command {}", command.name);
        }
        return Ok(ExitCode::from(dry_run_code(&existing)));
    }

    let request = Request {
        root,
        desktop_entry: if args.no_shortcut { Some(false) } else { None },
        command: if args.no_path { Some(false) } else { None },
    };
    let outcome = payload.install_into(&request, &xpack_core::NoProgress)?;

    xpack_core::outln!();
    xpack_core::outln!("Installed into {}", outcome.root.display());
    if let xpack_install::DesktopOutcome::Done(entries) = &outcome.desktop {
        for entry in entries {
            xpack_core::outln!("Added         {}", entry.display());
        }
    }
    xpack_core::outln!();
    xpack_core::outln!("Run it with:   {}", outcome.launcher.display());
    report_command(&outcome, payload.manifest());

    Ok(ExitCode::from(exit::INSTALLED))
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
