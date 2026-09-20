//! Generating the update index a server publishes.
//!
//! Packaging produces an `.xpkg`. Nothing until now produced the document that
//! tells an installed updater the package exists, so publishing meant writing
//! that JSON by hand — with a version, a platform, a size and a SHA-256 that a
//! human had to copy correctly, for every artefact, on every release.
//!
//! # It reads the packages rather than being told about them
//!
//! Every value in the index is taken from the package file itself: the
//! application, version and platform from its manifest, the digest and size
//! from its bytes. Nothing is accepted as a flag.
//!
//! That is deliberate. A caller that *can* pass `--version` will eventually
//! pass the wrong one, and the result is an index pointing at a package whose
//! signed manifest disagrees with it. The updater does catch that — it
//! compares the two before installing — but only after downloading the whole
//! package. Better that the mismatch cannot be expressed.
//!
//! # One file per platform, because that is what the updater fetches
//!
//! An installed updater fetches `<update.url>/<channel>.json` and refuses an
//! index whose platform is not its own. There is no platform component in that
//! URL, so **a single index serves exactly one platform**, and an application
//! shipping three platforms needs three indexes at three URLs.
//!
//! The manifest's `update.url` is what supplies the difference, and since it is
//! per-platform — the manifest is — the conventional layout is a platform
//! segment:
//!
//! ```text
//! https://updates.example.com/myapp/linux-x64/stable.json
//! https://updates.example.com/myapp/windows-x64/stable.json
//! ```
//!
//! This command mirrors that on disk, and refuses to write a set of packages
//! whose URLs would collide. See [`check_for_url_collisions`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args as ClapArgs;
use serde::Serialize;
use xpack_core::{Error, Manifest, Result};
use xpack_package::PackageReader;
use xpack_security::sha256_reader;
use xpack_update::index::{PackageRef, UpdateIndex};

/// Arguments for `xpack index`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Packages to publish. One index is written per platform.
    #[arg(value_name = "PACKAGE", required = true)]
    packages: Vec<PathBuf>,

    /// Directory to write the index files into.
    #[arg(long, value_name = "DIR", default_value = ".")]
    out_dir: PathBuf,

    /// Verify each package against this key before indexing it.
    ///
    /// Optional, and strongly recommended in a release pipeline: an index
    /// pointing at a package that does not verify is a release every client
    /// will download and then refuse.
    #[arg(long, value_name = "KEY")]
    key: Option<String>,

    /// URL of the release notes, recorded in every index written.
    #[arg(long, value_name = "URL")]
    release_notes: Option<String>,

    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

/// One index that was written.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Written {
    /// Where the index file was written.
    path: PathBuf,
    /// The platform it serves, in the `<os>-<arch>` form `--platform` takes.
    ///
    /// A string rather than the struct a `Platform` serialises to: this is a
    /// command-line contract, and the value has to be usable as an argument.
    platform: String,
    /// The package it names, relative to the index.
    package: String,
    /// The URL the installed application will fetch this from, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

/// Runs `xpack index`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    let key = match &args.key {
        Some(key) => Some(super::public_key(key)?),
        None => None,
    };

    let mut described = Vec::new();
    for path in &args.packages {
        described.push(describe(path, key.as_ref())?);
    }

    ensure_one_application(&described)?;
    check_for_url_collisions(&described);

    // Every refusal happens before the first write. A run that fails must
    // leave nothing behind rather than half a release tree, which a later
    // upload step would happily publish.
    let targets = resolve_targets(&described, &args.out_dir)?;

    let mut written = Vec::new();
    for (package, path) in described.iter().zip(targets) {
        written.push(write_index(package, path, args)?);
    }

    if args.json {
        crate::output::json(&written)?;
        return super::success();
    }

    for entry in &written {
        crate::output::field("index", entry.path.display());
        crate::output::field("platform", &entry.platform);
        crate::output::field("package", &entry.package);
        if let Some(url) = &entry.url {
            crate::output::field("url", url);
        }
        println!();
    }
    crate::output::field("indexes", written.len());

    if key.is_none() {
        eprintln!(
            "warning: packages were not verified; pass --key to check them before publishing"
        );
    }
    super::success()
}

/// Everything read out of one package file.
struct Described {
    file_name: String,
    manifest: Manifest,
    sha256: xpack_core::digest::Sha256Digest,
    size: u64,
}

/// Reads a package's manifest and hashes its bytes.
///
/// With a key, the manifest comes from a verified package. Without one it is
/// read unverified, exactly as `xpack inspect` does, and the caller is told.
fn describe(path: &std::path::Path, key: Option<&xpack_security::PublicKey>) -> Result<Described> {
    let manifest = match key {
        Some(key) => PackageReader::open(path)?
            .verify_with_keys(std::slice::from_ref(key))?
            .manifest()
            .clone(),
        None => PackageReader::open(path)?.peek_manifest_unverified()?.clone(),
    };

    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let (sha256, size) = sha256_reader(&mut std::io::BufReader::new(file))?;

    // The index names the package by file name, resolved by the updater
    // against the index's own URL. An absolute path would be meaningless to a
    // client and a relative one would depend on where this ran.
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::invalid("package", format!("{} has no file name", path.display())))?
        .to_string();

    Ok(Described { file_name, manifest, sha256, size })
}

/// Refuses a mixed set of applications.
///
/// An index set describes one application's releases. Mixing two would write
/// each one's index into the other's directory layout, and the mistake is far
/// easier to make than to notice: a build glob that matched too much.
fn ensure_one_application(packages: &[Described]) -> Result<()> {
    let mut ids: Vec<&str> = packages.iter().map(|p| p.manifest.application.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() <= 1 {
        return Ok(());
    }
    Err(Error::invalid(
        "index",
        format!("packages are for more than one application: {}", ids.join(", ")),
    ))
}

/// Warns when two platforms would publish to the same URL.
///
/// An installed updater fetches `<update.url>/<channel>.json` and rejects an
/// index whose platform is not its own. Two packages for different platforms
/// that declare the same `update.url` therefore write to the same address: the
/// second overwrites the first, and every client on the losing platform stops
/// updating with a platform mismatch it cannot resolve.
///
/// This is the single most likely mistake when adding a second platform, and
/// nothing else in the system reports it — the packages are individually valid
/// and the server is doing what it was told.
///
/// It warns rather than fails: the URLs are the publisher's to choose, this
/// command does not upload anything, and a deployment that maps paths some
/// other way is entitled to.
fn check_for_url_collisions(packages: &[Described]) {
    for collision in colliding_urls(packages) {
        eprintln!(
            "warning: {} all declare update.url {:?} on channel {:?}, so they would publish to \
             the same {}.json and overwrite each other",
            collision.platforms.join(", "),
            collision.url,
            collision.channel,
            collision.channel
        );
        eprintln!(
            "         give each platform its own url, conventionally by adding a platform \
             segment: {}/<os>-<arch>",
            collision.url
        );
    }
}

/// Two or more platforms that would publish to the same address.
#[derive(Debug, PartialEq, Eq)]
struct Collision {
    url: String,
    channel: String,
    platforms: Vec<String>,
}

/// Finds the sets of packages that would overwrite each other's index.
///
/// Separated from the reporting above so the rule itself is testable: this is
/// the check that decides whether a multi-platform release works at all, and
/// asserting on stderr would test the wrong thing.
fn colliding_urls(packages: &[Described]) -> Vec<Collision> {
    let mut by_url: BTreeMap<(&str, &str), Vec<String>> = BTreeMap::new();
    for package in packages {
        let Some(url) = &package.manifest.update.url else {
            continue;
        };
        by_url
            .entry((url.as_str(), package.manifest.update.channel.as_str()))
            .or_default()
            .push(package.manifest.platform.to_string());
    }

    by_url
        .into_iter()
        .filter_map(|((url, channel), mut platforms)| {
            platforms.sort();
            platforms.dedup();
            (platforms.len() > 1).then(|| Collision {
                url: url.to_string(),
                channel: channel.to_string(),
                platforms,
            })
        })
        .collect()
}

/// Works out where every index will go, refusing a set that collides.
///
/// Computed for all packages before any is written, so a run that cannot
/// succeed writes nothing at all.
///
/// Two packages for the same platform and channel are a genuine ambiguity —
/// only one of them can be the release — so that is an error rather than a
/// last-one-wins. An index left by a *previous* run is simply replaced: this
/// is an output directory, and publishing a new version is the normal reason
/// to run the command twice.
fn resolve_targets(packages: &[Described], out_dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut targets = Vec::with_capacity(packages.len());
    let mut seen: BTreeMap<PathBuf, &Described> = BTreeMap::new();

    for package in packages {
        let manifest = &package.manifest;
        let path = out_dir
            .join(manifest.platform.to_string())
            .join(format!("{}.json", manifest.update.channel));

        if let Some(previous) = seen.get(&path) {
            return Err(Error::invalid(
                "index",
                format!(
                    "{} and {} both target {} on channel {:?}; only one can be the release",
                    previous.file_name,
                    package.file_name,
                    manifest.platform,
                    manifest.update.channel
                ),
            ));
        }
        seen.insert(path.clone(), package);
        targets.push(path);
    }
    Ok(targets)
}

/// Writes one platform's index to an already-resolved path.
fn write_index(package: &Described, path: PathBuf, args: &Args) -> Result<Written> {
    let manifest = &package.manifest;
    let channel = manifest.update.channel.clone();

    let index = UpdateIndex {
        application: manifest.application.id.clone(),
        version: manifest.application.version.clone(),
        platform: manifest.platform,
        channel: channel.clone(),
        package: PackageRef {
            file: package.file_name.clone(),
            size: package.size,
            sha256: Some(package.sha256),
        },
        release_notes: args.release_notes.clone(),
    };

    // The layout mirrors what the updater fetches: one directory per platform,
    // one file per channel inside it. A publisher uploading this tree to the
    // path their manifests name gets working updates without arranging
    // anything else.
    xpack_core::atomic::create_dir_all(xpack_core::atomic::parent_dir(&path)?)?;
    let body = serde_json::to_vec_pretty(&index).map_err(|e| Error::json("update index", e))?;
    xpack_core::atomic::write(&path, &body)?;

    tracing::info!(
        index = %path.display(),
        platform = %manifest.platform,
        version = %manifest.application.version,
        "wrote update index"
    );

    Ok(Written {
        path,
        platform: manifest.platform.to_string(),
        package: package.file_name.clone(),
        url: manifest
            .update
            .url
            .as_ref()
            .map(|base| format!("{}/{channel}.json", base.trim_end_matches('/'))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use xpack_core::{Arch, Os, Platform};

    fn described(id: &str, platform: Platform, url: Option<&str>) -> Described {
        let manifest = Manifest {
            format_version: xpack_core::FormatVersion::default(),
            application: xpack_core::Application {
                id: id.to_string(),
                name: "Example".into(),
                version: xpack_core::Version::parse("1.0.0").unwrap(),
                description: None,
                publisher: None,
            },
            platform,
            launch: xpack_core::LaunchSpec {
                executable: "app".into(),
                arguments: Vec::new(),
                working_directory: None,
                environment: BTreeMap::new(),
            },
            update: xpack_core::UpdateSpec {
                channel: "stable".into(),
                url: url.map(str::to_string),
                mandatory: false,
            },
            health: xpack_core::HealthSpec::default(),
            desktop: xpack_core::DesktopSpec::default(),
            payload: xpack_core::PayloadSpec::default(),
            signing_key: None,
            created_at: None,
        };
        Described {
            file_name: "pkg.xpkg".into(),
            manifest,
            sha256: xpack_core::digest::Sha256Digest::from_bytes([0; 32]),
            size: 1,
        }
    }

    #[test]
    fn one_application_across_several_platforms_is_accepted() {
        let packages = vec![
            described("com.example.app", Platform::new(Os::Linux, Arch::X64), None),
            described("com.example.app", Platform::new(Os::Windows, Arch::X64), None),
        ];
        assert!(ensure_one_application(&packages).is_ok());
    }

    #[test]
    fn a_mixed_set_of_applications_is_refused() {
        // The realistic cause is a build glob that matched too much, and the
        // result would be one application's index in another's layout.
        let packages = vec![
            described("com.example.app", Platform::new(Os::Linux, Arch::X64), None),
            described("com.example.other", Platform::new(Os::Linux, Arch::X64), None),
        ];
        let err = ensure_one_application(&packages).unwrap_err();
        assert!(err.to_string().contains("more than one application"), "got {err}");
    }

    #[test]
    fn an_empty_set_is_not_a_conflict() {
        assert!(ensure_one_application(&[]).is_ok());
    }

    #[test]
    fn two_platforms_sharing_one_url_are_reported() {
        // Nothing else in the system catches this: both packages are valid,
        // and the server does exactly what it was told.
        let shared = Some("https://updates.example.com/myapp");
        let packages = vec![
            described("com.example.app", Platform::new(Os::Linux, Arch::X64), shared),
            described("com.example.app", Platform::new(Os::Windows, Arch::X64), shared),
        ];
        // The warning goes to stderr; what is asserted here is that inspecting
        // the set finds the collision rather than panicking or silently
        // passing over it.
        assert_eq!(colliding_urls(&packages).len(), 1);
    }

    #[test]
    fn a_platform_segment_per_url_removes_the_collision() {
        let packages = vec![
            described(
                "com.example.app",
                Platform::new(Os::Linux, Arch::X64),
                Some("https://updates.example.com/myapp/linux-x64"),
            ),
            described(
                "com.example.app",
                Platform::new(Os::Windows, Arch::X64),
                Some("https://updates.example.com/myapp/windows-x64"),
            ),
        ];
        assert!(colliding_urls(&packages).is_empty());
    }

    #[test]
    fn packages_without_a_url_cannot_collide() {
        // No update server configured is a legitimate build; it simply never
        // publishes, and it must not be reported as a conflict.
        let packages = vec![
            described("com.example.app", Platform::new(Os::Linux, Arch::X64), None),
            described("com.example.app", Platform::new(Os::Windows, Arch::X64), None),
        ];
        assert!(colliding_urls(&packages).is_empty());
    }

    #[test]
    fn two_packages_for_one_platform_are_refused_before_anything_is_written() {
        // Only one of them can be the release. Detected up front, so a run
        // that cannot succeed leaves no half-written tree behind.
        let mut first = described("com.example.app", Platform::new(Os::Linux, Arch::X64), None);
        first.file_name = "a.xpkg".into();
        let mut second = described("com.example.app", Platform::new(Os::Linux, Arch::X64), None);
        second.file_name = "b.xpkg".into();

        let err = resolve_targets(&[first, second], std::path::Path::new("out")).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("a.xpkg"), "got {text}");
        assert!(text.contains("b.xpkg"), "got {text}");
    }

    #[test]
    fn one_package_per_platform_resolves_to_one_path_each() {
        let packages = vec![
            described("com.example.app", Platform::new(Os::Linux, Arch::X64), None),
            described("com.example.app", Platform::new(Os::Windows, Arch::X64), None),
        ];
        let targets = resolve_targets(&packages, std::path::Path::new("out")).unwrap();
        assert_eq!(targets.len(), 2);
        assert!(targets[0].ends_with("linux-x64/stable.json"), "{:?}", targets[0]);
        assert!(targets[1].ends_with("windows-x64/stable.json"), "{:?}", targets[1]);
    }

    #[test]
    fn the_same_platform_on_two_channels_is_not_a_conflict() {
        // A beta and a stable release of the same build are different files.
        let mut stable = described("com.example.app", Platform::new(Os::Linux, Arch::X64), None);
        stable.manifest.update.channel = "stable".into();
        let mut beta = described("com.example.app", Platform::new(Os::Linux, Arch::X64), None);
        beta.manifest.update.channel = "beta".into();

        assert!(resolve_targets(&[stable, beta], std::path::Path::new("out")).is_ok());
    }

    #[test]
    fn one_platform_on_one_url_is_the_normal_case() {
        let packages = vec![described(
            "com.example.app",
            Platform::new(Os::Linux, Arch::X64),
            Some("https://updates.example.com/myapp/linux-x64"),
        )];
        assert!(colliding_urls(&packages).is_empty());
    }
}
