//! Installing a package.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageReader;

use super::Context;

/// Arguments for `xpack install`.
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

    let options =
        InstallOptions { allow_downgrade: args.allow_downgrade, activate: !args.no_activate };
    let installed = Installer::new(&lock).install(&mut verified, &options)?;

    if !installed.recovery.is_empty() {
        eprintln!("note: recovered from an interrupted operation before installing");
    }

    crate::output::field("application", &application_id);
    crate::output::field("version", &installed.version);
    crate::output::field("signed by", signed_by);
    crate::output::field("active", installed.activated);
    crate::output::field("location", lock.paths().version_dir(&installed.version).display());

    if matches!(decision, TrustDecision::OnFirstUse) {
        eprintln!();
        eprintln!(
            "warning: the signing key was pinned on first use. Every later update must be \
             signed by the same key, but this install itself was trusted on sight."
        );
    }

    super::success()
}
