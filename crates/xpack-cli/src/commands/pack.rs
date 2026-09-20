//! Building a signed package.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use serde::Serialize;
use xpack_core::digest::Sha256Digest;
use xpack_core::{Platform, Result, Version};
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

    /// Platform to build for, as `<os>-<arch>`, e.g. `linux-x64`.
    ///
    /// Overrides the project configuration, which in turn overrides the host.
    /// This is what lets one configuration produce every artefact a release
    /// needs without a build rewriting its own config file between runs.
    #[arg(long, value_name = "PLATFORM")]
    platform: Option<Platform>,

    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

/// What `xpack pack --json` prints.
///
/// A separate type rather than an ad-hoc `serde_json::json!`, so the shape is
/// declared in one place and a field cannot be renamed without the compiler
/// noticing. Everything a caller needs to publish the package is here: the two
/// values an update index requires are `sha256` and `size`.
///
/// # Why `platform` is a string here and a struct in the manifest
///
/// A `Platform` serialises as `{"os": …, "arch": …}`, which is the on-disk
/// manifest format and cannot change. This is a command-line contract, and a
/// caller reading it needs to be able to hand the value straight back to
/// `--platform`, which takes `linux-x64`. Emitting the struct would mean every
/// integration reassembling the string itself, and getting the separator wrong
/// once.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackReport<'a> {
    package: &'a std::path::Path,
    application: &'a str,
    version: &'a Version,
    platform: String,
    files: usize,
    size: u64,
    sha256: &'a Sha256Digest,
    signed_by: String,
}

/// Runs `xpack pack`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let config = ProjectConfig::load(&args.config)?;
    let key = KeyPair::load(&args.key)?;

    // `--platform` wins over the configuration, which wins over the host. The
    // host is only a convenience for someone packaging what they are standing
    // on; a release build states its target.
    let manifest = match args.platform {
        Some(platform) => config.to_manifest(platform),
        None => config.to_manifest(Platform::host()?),
    };

    let output = match &args.out {
        Some(path) => path.clone(),
        None => args.out_dir.join(manifest.package_file_name()),
    };

    // Second layer, by path rather than by content. `xpack-package` refuses a
    // recognisable xPack key file wherever it appears; this refuses *the key
    // being used right now* if it sits inside the tree being packaged, whatever
    // format it is in and whatever it is called.
    ensure_key_is_outside_payload(&args.key, &args.payload)?;

    let packed = PackageBuilder::new(&args.payload, manifest).build(&output, &key)?;

    if args.json {
        crate::output::json(&PackReport {
            package: &packed.path,
            application: &packed.manifest.application.id,
            version: &packed.manifest.application.version,
            platform: packed.manifest.platform.to_string(),
            files: packed.manifest.payload.files.len(),
            size: packed.size,
            sha256: &packed.sha256,
            signed_by: key.public().fingerprint(),
        })?;
        return super::success();
    }

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

/// Refuses to package the directory that holds the signing key.
///
/// `xpack keygen` writes `xpack-signing.json` into the working directory and
/// `xpack pack .` packages the working directory, so the two defaults compose
/// into publishing the private key that authorises every future update — in a
/// package whose signature verifies perfectly.
///
/// Compared after canonicalising, because `./key.json` and `key.json` and a
/// path reached through a symlink are the same file, and the answer decides
/// whether a secret is published. A path that cannot be canonicalised is left
/// alone: it does not exist, so it cannot be inside the payload, and the build
/// will fail for its own reasons a moment later.
fn ensure_key_is_outside_payload(key: &std::path::Path, payload: &std::path::Path) -> Result<()> {
    let (Ok(key), Ok(payload)) = (key.canonicalize(), payload.canonicalize()) else {
        return Ok(());
    };
    if !key.starts_with(&payload) {
        return Ok(());
    }
    Err(xpack_core::Error::invalid(
        "key",
        format!(
            "the signing key {} is inside the payload directory {} and would be published with \
             the application; move it outside the tree being packaged",
            key.display(),
            payload.display()
        ),
    ))
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
