//! Describing a package without trusting it.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::Result;
use xpack_package::PackageReader;

/// Arguments for `xpack inspect`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Package to describe.
    #[arg(value_name = "PACKAGE")]
    package: PathBuf,

    /// Emit the manifest as JSON.
    #[arg(long)]
    json: bool,
}

/// Runs `xpack inspect`.
///
/// Deliberately does **not** verify. Describing an untrusted file is the point
/// — it is how an operator decides whether to trust it — so the output says so
/// rather than letting a reader assume otherwise.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let mut reader = PackageReader::open(&args.package)?;
    let manifest = reader.peek_manifest_unverified()?;

    if args.json {
        crate::output::json(&manifest)?;
        xpack_core::errln!(
            "warning: this manifest has NOT been verified; use `xpack verify` to check it"
        );
        return super::success();
    }

    crate::output::field("application", &manifest.application.id);
    crate::output::field("name", &manifest.application.name);
    crate::output::field("version", &manifest.application.version);
    crate::output::field("platform", manifest.platform);
    crate::output::field("format", manifest.format_version.0);
    crate::output::field("launch", &manifest.launch.executable);
    if !manifest.launch.arguments.is_empty() {
        crate::output::field("arguments", manifest.launch.arguments.join(" "));
    }
    crate::output::field("files", manifest.payload.files.len());
    crate::output::field("payload size", super::pack::format_size(manifest.payload.total_size));
    if let Some(declared) = &manifest.signing_key {
        crate::output::field("declares key", declared);
    }

    xpack_core::errln!();
    xpack_core::errln!(
        "warning: nothing here has been verified. A package can claim anything until its \
         signature is checked against a key you already trust."
    );
    super::success()
}
