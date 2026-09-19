//! Building a signed package.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use xpack_core::{Platform, Result};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

use crate::config::ProjectConfig;

/// Arguments for `xpack pack`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Directory whose contents become the payload.
    #[arg(value_name = "PAYLOAD_DIR")]
    payload: PathBuf,

    /// Project configuration.
    #[arg(long, value_name = "FILE", default_value = "xpack.json")]
    config: PathBuf,

    /// Private signing key.
    #[arg(long, value_name = "FILE")]
    key: PathBuf,

    /// Output path. Defaults to a name derived from the manifest.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,

    /// Directory for the derived output name.
    #[arg(long, value_name = "DIR", default_value = ".")]
    out_dir: PathBuf,
}

/// Runs `xpack pack`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let config = ProjectConfig::load(&args.config)?;
    let key = KeyPair::load(&args.key)?;
    let manifest = config.to_manifest(Platform::host()?);

    let output = match &args.out {
        Some(path) => path.clone(),
        None => args.out_dir.join(manifest.package_file_name()),
    };

    let packed = PackageBuilder::new(&args.payload, manifest).build(&output, &key)?;

    crate::output::field("package", packed.path.display());
    crate::output::field("application", &packed.manifest.application.id);
    crate::output::field("version", &packed.manifest.application.version);
    crate::output::field("platform", packed.manifest.platform);
    crate::output::field("files", packed.manifest.payload.files.len());
    crate::output::field("size", format_size(packed.size));
    crate::output::field("sha256", packed.sha256);
    crate::output::field("signed by", key.public().fingerprint());

    super::success()
}

/// Formats a byte count for a human reading a terminal.
///
/// Precision loss past 2^53 bytes is irrelevant: the value is rounded to one
/// decimal place for display and never used for arithmetic.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}
