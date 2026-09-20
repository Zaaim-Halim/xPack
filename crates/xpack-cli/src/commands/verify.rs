//! Checking a package's signature.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_package::PackageReader;

/// Arguments for `xpack verify`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Package to verify.
    #[arg(value_name = "PACKAGE")]
    package: PathBuf,

    /// Public key file, or a 64-character hex key.
    #[arg(long, value_name = "KEY")]
    key: String,
}

/// Runs `xpack verify`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let key = super::public_key(&args.key)?;
    let mut verified =
        PackageReader::open(&args.package)?.verify_with_keys(std::slice::from_ref(&key))?;

    // The signature proves the manifest; this proves the payload matches it.
    // Without it a package whose contents were replaced still reports as
    // verified, because its manifest is untouched and correctly signed — and
    // this command exists precisely to be trusted as a gate.
    let files = verified.verify_payload_digests()?;
    let manifest = verified.manifest();

    crate::output::field("package", args.package.display());
    crate::output::field("application", &manifest.application.id);
    crate::output::field("version", &manifest.application.version);
    crate::output::field("platform", manifest.platform);
    crate::output::field("signed by", key.fingerprint());
    crate::output::field("files", files);
    println!();
    println!("signature verified, and every file matches the signed manifest");

    // Verification proves who signed it, not that it suits this machine.
    if let Ok(host) = xpack_core::Platform::host()
        && !manifest.platform.accepts(host)
    {
        eprintln!(
            "note: this package targets {} and cannot be installed on {host}",
            manifest.platform
        );
    }

    super::success()
}
