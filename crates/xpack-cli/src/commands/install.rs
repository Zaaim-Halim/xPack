//! Installing a package.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_install::{InstallOptions, Installer, LauncherOutcome, TrustDecision, open_and_verify};
use xpack_package::PackageReader;

use super::Context;

/// Arguments for `xpack install`.
///
/// The bool count trips a lint meant for domain types, where several flags
/// usually mean a missing enum. These are command-line switches: each one is
/// independently settable by a user and clap requires exactly this shape.
#[allow(clippy::struct_excessive_bools)]
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Package to install.
    #[arg(value_name = "PACKAGE")]
    package: PathBuf,

    /// Public key to trust, as a file or a 64-character hex key.
    #[arg(long, value_name = "KEY", conflicts_with = "trust_on_first_use")]
    trust: Option<String>,

    /// Pin whichever key signed this package.
    ///
    /// Trusts the first download and nothing after it.
    #[arg(long, conflicts_with = "trust")]
    trust_on_first_use: bool,

    /// Do not make the installed version active.
    #[arg(long)]
    no_activate: bool,

    /// Permit installing a version that is not newer than the active one.
    #[arg(long)]
    allow_downgrade: bool,

    /// Launcher binary to place in the installation root.
    ///
    /// Defaults to the `xpack-launcher` sitting beside this executable.
    #[arg(long, value_name = "FILE", conflicts_with = "no_launcher")]
    launcher: Option<PathBuf>,

    /// Install without a launcher, leaving the application with no entry point.
    #[arg(long, conflicts_with = "launcher")]
    no_launcher: bool,
}

impl Args {
    /// The installation this command will act on.
    ///
    /// Read from the package, unverified, because the installation is not
    /// known any other way and a log destination has to be chosen before any
    /// work begins.
    ///
    /// Using unverified data here is safe precisely because of what it is used
    /// for: at worst a log lands in the wrong directory. It cannot affect what
    /// gets installed, because the install path reads the package again and
    /// verifies it before trusting anything.
    pub(crate) fn application_id(&self) -> Option<String> {
        let mut reader = PackageReader::open(&self.package).ok()?;
        Some(reader.peek_manifest_unverified().ok()?.application.id)
    }
}

/// Runs `xpack install`.
pub(crate) fn run(args: &Args, context: &Context) -> Result<ExitCode> {
    // The application id decides which installation this belongs to, so it has
    // to be read before the package can be verified against that
    // installation's pinned keys.
    let application_id = {
        let mut reader = PackageReader::open(&args.package)?;
        reader.peek_manifest_unverified()?.application.id
    };

    let lock = context.lock(&application_id)?;

    let decision = match (&args.trust, args.trust_on_first_use) {
        (Some(key), _) => TrustDecision::Explicit(super::public_key(key)?),
        (None, true) => TrustDecision::OnFirstUse,
        (None, false) => TrustDecision::UsePinned,
    };

    let mut verified = open_and_verify(&args.package, &lock, &decision)?;
    let signed_by = verified.signing_key().fingerprint();

    let launcher = resolve_launcher(args)?;
    // The updater follows the launcher: an installation with an entry point
    // but no way to learn about updates is only half of what was asked for.
    // Only placed where it means something. On Unix the windowed build is an
    // exact duplicate of the console one, and installing it would double the
    // launcher bytes in every installation to no effect.
    let gui_launcher = if args.no_launcher || !xpack_core::HAS_WINDOWED_LAUNCHER {
        None
    } else {
        super::default_gui_launcher()
    };
    let updater = if args.no_launcher { None } else { super::default_updater() };
    let uninstaller = if args.no_launcher { None } else { super::default_uninstaller() };
    // Offered unconditionally; the installer places it only for a package that
    // asked to prompt its users.
    let notifier = if args.no_launcher { None } else { super::default_notifier() };
    let options = InstallOptions {
        allow_downgrade: args.allow_downgrade,
        activate: !args.no_activate,
        launcher,
        gui_launcher,
        updater,
        uninstaller,
        notifier,
        // The user's own directories: this is a real installation.
        desktop_roots: None,
        // The manifest's request, or a choice recorded at an earlier install.
        desktop_entry: None,
    };
    let installed = Installer::new(&lock).install(&mut verified, &options)?;

    if !installed.recovery.is_empty() {
        eprintln!("note: recovered from an interrupted operation before installing");
    }

    crate::output::field("application", &application_id);
    crate::output::field("version", &installed.version);
    crate::output::field("signed by", signed_by);
    crate::output::field("active", installed.activated);
    crate::output::field("location", lock.paths().version_dir(&installed.version).display());
    // Under the names this installation gave them, which are the names of
    // the files that are actually there. Reporting the ones this binary was
    // built expecting would print four paths a user cannot open.
    let names = lock
        .load_state()
        .map_or(xpack_core::BinaryNames::Xpack, |state| state.value.binary_names());
    let paths = lock.paths();
    report_binary("launcher", installed.launcher, &paths.launcher_file_named(&names));
    report_binary(
        "windowed launcher",
        installed.gui_launcher,
        &paths.gui_launcher_file_named(&names),
    );
    report_binary("updater", installed.updater, &paths.updater_file_named(&names));
    report_binary("uninstaller", installed.uninstaller, &paths.uninstaller_file_named(&names));

    if matches!(decision, TrustDecision::OnFirstUse) {
        eprintln!();
        eprintln!(
            "warning: the signing key was pinned on first use. Every later update must be \
             signed by the same key, but this install itself was trusted on sight."
        );
    }

    super::success()
}

/// Decides which launcher binary, if any, to place in the installation.
///
/// An explicit path is used as given and fails loudly when it is missing,
/// because a caller who named a file expects that file.
///
/// Otherwise the default is the `xpack-launcher` built or shipped beside this
/// executable. That is a convenience for the common cases — a developer
/// running out of a build directory, a bootstrap installer shipping both
/// binaries together — and its absence is not an error: an installation
/// without a launcher is still valid, just without an entry point. It says so
/// rather than failing, so `xpack install` keeps working wherever the CLI has
/// been deployed on its own.
fn resolve_launcher(args: &Args) -> Result<Option<PathBuf>> {
    if args.no_launcher {
        return Ok(None);
    }
    if let Some(path) = &args.launcher {
        if !path.is_file() {
            return Err(xpack_core::Error::invalid(
                "launcher",
                format!("{} does not exist", path.display()),
            ));
        }
        return Ok(Some(path.clone()));
    }

    let Some(sibling) = super::default_launcher() else {
        eprintln!("note: no launcher was installed; pass --launcher to supply one");
        return Ok(None);
    };
    Ok(Some(sibling))
}

/// Prints what happened to one of the binaries placed in the installation.
fn report_binary(what: &str, outcome: Option<LauncherOutcome>, path: &std::path::Path) {
    match outcome {
        Some(LauncherOutcome::Installed) => crate::output::field(what, path.display()),
        Some(LauncherOutcome::AlreadyPresent) => crate::output::field(what, "already present"),
        None => crate::output::field(what, "none"),
    }
}
