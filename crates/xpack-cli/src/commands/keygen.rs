//! Generating a signing key pair.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_security::KeyPair;

/// Arguments for `xpack keygen`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Where to write the private key. Keep this secret.
    #[arg(long, value_name = "FILE", default_value = "xpack-signing.json")]
    out: PathBuf,

    /// Where to write the public key. Defaults to `<OUT>.pub.json`.
    #[arg(long, value_name = "FILE")]
    public: Option<PathBuf>,

    /// Overwrite an existing key file.
    #[arg(long)]
    force: bool,
}

/// Runs `xpack keygen`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    // Overwriting a signing key destroys the ability to publish updates that
    // existing installations will accept. Every one of them would have to be
    // re-pinned by hand, which for a deployed application means telling users
    // to reinstall. It is not something to do by accident.
    if args.out.exists() && !args.force {
        return Err(xpack_core::Error::invalid(
            "keygen",
            format!(
                "{} already exists. Overwriting a signing key makes every existing \
                 installation unable to accept updates; pass --force only if that is intended",
                args.out.display()
            ),
        ));
    }

    let pair = KeyPair::generate()?;
    pair.save(&args.out)?;

    let public_path = args.public.clone().unwrap_or_else(|| args.out.with_extension("pub.json"));
    pair.public().save(&public_path)?;

    crate::output::field("private key", args.out.display());
    crate::output::field("public key", public_path.display());
    crate::output::field("fingerprint", pair.public().fingerprint());
    println!();
    println!("{}", pair.public().to_hex());
    println!();
    eprintln!(
        "Keep the private key secret and out of version control. Distribute the public key \
         so installations can pin it."
    );
    #[cfg(not(unix))]
    eprintln!(
        "note: file permissions were not restricted on this platform; store the private key \
         somewhere only you can read."
    );

    super::success()
}
