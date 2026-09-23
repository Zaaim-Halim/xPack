//! Building a differential update.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use serde::Serialize;
use xpack_core::Result;
use xpack_package::delta;

/// Arguments for `xpack delta`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// The published package a user already has.
    #[arg(value_name = "FROM")]
    base: PathBuf,

    /// The published package they should end up with.
    #[arg(value_name = "TO")]
    target: PathBuf,

    /// Where to write the delta.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,

    /// Directory for the derived output name.
    #[arg(long, value_name = "DIR", default_value = ".")]
    out_dir: PathBuf,

    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

/// What `xpack delta --json` prints.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeltaReport<'a> {
    delta: &'a std::path::Path,
    application: &'a str,
    from: String,
    to: String,
    changed: usize,
    reused: usize,
    size: u64,
    full_size: u64,
    percentage_of_full: u64,
}

/// Runs `xpack delta`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let application = {
        let mut reader = xpack_package::PackageReader::open(&args.target)?;
        reader.peek_manifest_unverified()?.application.id
    };

    let output = match &args.out {
        Some(path) => path.clone(),
        None => args.out_dir.join(default_name(&args.base, &args.target)?),
    };

    let built = delta::build(&args.base, &args.target, &output)?;

    if args.json {
        crate::output::json(&DeltaReport {
            delta: &built.path,
            application: &application,
            from: built.base_version.to_string(),
            to: built.target_version.to_string(),
            changed: built.changed,
            reused: built.reused,
            size: built.size,
            full_size: built.full_size,
            percentage_of_full: built.percentage_of_full(),
        })?;
        return super::success();
    }

    crate::output::field("delta", built.path.display());
    crate::output::field("application", &application);
    crate::output::field("from", &built.base_version);
    crate::output::field("to", &built.target_version);
    crate::output::field("changed", built.changed);
    crate::output::field("reused", built.reused);
    crate::output::field("size", super::pack::format_size(built.size));
    crate::output::field(
        "full package",
        format!(
            "{} ({}% of it)",
            super::pack::format_size(built.full_size),
            built.percentage_of_full()
        ),
    );

    xpack_core::errln!();
    xpack_core::errln!(
        "note: publish this beside the full package. A delta is an optimisation — a client \
         that cannot use one falls back to the full package."
    );
    super::success()
}

/// `<Name>-<from>-to-<to>-<platform>.xpkgd`.
///
/// Both versions are in the name because a delta is only usable by someone on
/// exactly the version it was built from, and a release publishes several.
fn default_name(base: &std::path::Path, target: &std::path::Path) -> Result<String> {
    let base_manifest = xpack_package::PackageReader::open(base)?.peek_manifest_unverified()?;
    let target_manifest = xpack_package::PackageReader::open(target)?.peek_manifest_unverified()?;

    let stem: String = target_manifest
        .application
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let stem = stem.trim_matches('-').to_string();
    let stem = if stem.is_empty() { "application".to_string() } else { stem };

    Ok(format!(
        "{stem}-{}-to-{}-{}.{}",
        base_manifest.application.version.to_directory_name(),
        target_manifest.application.version.to_directory_name(),
        target_manifest.platform,
        delta::DELTA_EXTENSION
    ))
}
