//! Assembling a self-contained installer.
//!
//! Takes a package and the xPack runtime binaries and produces one artefact a
//! user runs on a machine that has never heard of xPack.
//!
//! # Two layouts, chosen by the target platform
//!
//! | Target | Produced |
//! | --- | --- |
//! | Windows, Linux | one executable: stub, payload, trailer |
//! | macOS | an `.app` bundle: pristine stub, payload in `Resources` |
//!
//! Appending to a Mach-O leaves a binary that still runs but whose code
//! signature no longer covers the whole file, and re-signing does not repair
//! it. A macOS installer has to satisfy Gatekeeper once it has been
//! downloaded, and an unsignable one never will — so on macOS the executable
//! is left untouched and the payload sits beside it, which is the arrangement
//! Apple's tooling expects regardless.
//!
//! # Signing is the publisher's, and comes after this
//!
//! Appending invalidates an Authenticode signature just as it does a Mach-O
//! one, so the stub must be signed *after* assembly, never before. That is the
//! normal order: sign the artefact you ship. This command does no signing —
//! the certificates belong to the publisher, not to xPack — and the Ed25519
//! signature it does care about is the one already inside the package.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args as ClapArgs;
use serde::Serialize;
use xpack_core::{Error, Os, Result};
use xpack_installer::bundle::{InstallPlan, MAGIC, SIDECAR_NAME, TRAILER_LEN, Trailer};
use xpack_package::PackageReader;

/// Arguments for `xpack installer`.
#[derive(ClapArgs)]
pub(crate) struct Args {
    /// Package the installer will install.
    #[arg(value_name = "PACKAGE")]
    package: PathBuf,

    /// The `xpack-installer` stub to build on.
    ///
    /// Defaults to the one beside this executable. It must be built for the
    /// platform the installer targets, which is why cross-building an
    /// installer means supplying the right stub rather than a flag.
    #[arg(long, value_name = "FILE")]
    stub: Option<PathBuf>,

    /// Runtime binaries to place in the installation.
    ///
    /// Repeatable. Defaults to the launcher, updater and uninstaller sitting
    /// beside this executable, plus the windowed launcher when targeting
    /// Windows.
    #[arg(long = "binary", value_name = "FILE")]
    binaries: Vec<PathBuf>,

    /// Where to write the installer.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,

    /// Directory for the derived output name.
    #[arg(long, value_name = "DIR", default_value = ".")]
    out_dir: PathBuf,

    /// Do not make the installed version active.
    #[arg(long)]
    no_activate: bool,

    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

/// What `xpack installer --json` prints.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallerReport<'a> {
    installer: &'a Path,
    layout: &'a str,
    application: &'a str,
    version: String,
    platform: String,
    size: u64,
    signed_by: String,
}

/// Runs `xpack installer`.
pub(crate) fn run(args: &Args) -> Result<ExitCode> {
    // Read unverified, exactly as `inspect` does: this describes the package
    // so the installer can report it before anything is trusted. What makes
    // the artefact trustworthy is the key pinned into the plan, and the
    // signature the *installer* checks at install time against it.
    let mut reader = PackageReader::open(&args.package)?;
    let manifest = reader.peek_manifest_unverified()?.clone();

    let signing_key = manifest.signing_key.clone().ok_or_else(|| {
        Error::invalid(
            "package",
            "declares no signing key, so an installer built from it could not pin one; \
             repack it with a current xpack",
        )
    })?;

    let plan = InstallPlan {
        format_version: InstallPlan::CURRENT_VERSION,
        application_id: manifest.application.id.clone(),
        application_name: manifest.application.name.clone(),
        version: manifest.application.version.to_string(),
        signing_key: signing_key.clone(),
        activate: !args.no_activate,
    };

    let stub = match &args.stub {
        Some(path) => path.clone(),
        None => super::sibling_binary_required("xpack-installer")?,
    };
    let binaries = resolve_binaries(args, manifest.platform.os)?;
    let payload = xpack_installer::bundle::build(&plan, &args.package, &binaries)?;

    let target = manifest.platform.os;
    let output = match &args.out {
        Some(path) => path.clone(),
        None => args.out_dir.join(default_name(&manifest, target)),
    };

    let layout = match target {
        // Appending to a Mach-O breaks its code signature beyond repair.
        Os::Macos => {
            write_bundle(&output, &stub, &payload, &manifest.application.name)?;
            "bundle"
        }
        Os::Windows | Os::Linux => {
            write_appended(&output, &stub, &payload)?;
            "executable"
        }
    };

    let size = total_size(&output);

    if args.json {
        crate::output::json(&InstallerReport {
            installer: &output,
            layout,
            application: &manifest.application.id,
            version: manifest.application.version.to_string(),
            platform: manifest.platform.to_string(),
            size,
            signed_by: signing_key,
        })?;
        return super::success();
    }

    crate::output::field("installer", output.display());
    crate::output::field("layout", layout);
    crate::output::field("application", &manifest.application.id);
    crate::output::field("version", &manifest.application.version);
    crate::output::field("platform", manifest.platform);
    crate::output::field("size", super::pack::format_size(size));
    crate::output::field("binaries", binaries.len());

    if target == Os::Macos {
        eprintln!();
        eprintln!(
            "note: sign and notarise this bundle before distributing it; macOS blocks an \
             unsigned downloaded installer."
        );
    } else {
        eprintln!();
        eprintln!(
            "note: sign this installer if you distribute it, and sign it *after* this step — \
             appending the payload invalidates a signature applied to the stub."
        );
    }

    super::success()
}

/// The runtime binaries to ship, defaulting to those beside this executable.
fn resolve_binaries(args: &Args, target: Os) -> Result<Vec<PathBuf>> {
    if !args.binaries.is_empty() {
        for path in &args.binaries {
            if !path.is_file() {
                return Err(Error::invalid("binary", format!("{} does not exist", path.display())));
            }
        }
        return Ok(args.binaries.clone());
    }

    let mut wanted = vec!["xpack-launcher", "xpack-updater", "xpack-uninstaller"];
    // Only Windows distinguishes a windowed build; elsewhere it is a duplicate
    // of the console one and installing it would double the launcher bytes.
    if target == Os::Windows {
        wanted.push("xpack-launcherw");
    }

    let mut found = Vec::new();
    for name in wanted {
        found.push(super::sibling_binary_required(name)?);
    }
    Ok(found)
}

/// Writes a stub with the payload and trailer appended.
fn write_appended(output: &Path, stub: &Path, payload: &[u8]) -> Result<()> {
    let mut bytes = std::fs::read(stub).map_err(|e| Error::io(stub, e))?;
    if bytes.is_empty() {
        return Err(Error::invalid("stub", format!("{} is empty", stub.display())));
    }
    // A stub that already carries a payload would end up with two, and the
    // trailer at the end would name the second while the first sat in the
    // middle of the file as dead weight.
    if bytes.len() > TRAILER_LEN && bytes[bytes.len() - TRAILER_LEN..][..8] == MAGIC {
        return Err(Error::invalid(
            "stub",
            format!("{} is already a built installer, not a stub", stub.display()),
        ));
    }

    let trailer = Trailer {
        payload_len: payload.len() as u64,
        payload_sha256: xpack_security::sha256(payload),
    };

    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&trailer.to_bytes());

    if let Some(parent) = output.parent() {
        xpack_core::atomic::create_dir_all(parent)?;
    }
    xpack_core::atomic::write(output, &bytes)?;
    set_executable(output)
}

/// Writes a macOS application bundle with the payload beside a pristine stub.
fn write_bundle(output: &Path, stub: &Path, payload: &[u8], name: &str) -> Result<()> {
    let contents = output.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    for directory in [&macos, &resources] {
        xpack_core::atomic::create_dir_all(directory)?;
    }

    let executable = installer_display_name(name);
    let stub_bytes = std::fs::read(stub).map_err(|e| Error::io(stub, e))?;
    let destination = macos.join(&executable);
    xpack_core::atomic::write(&destination, &stub_bytes)?;
    set_executable(&destination)?;

    xpack_core::atomic::write(&resources.join(SIDECAR_NAME), payload)?;
    xpack_core::atomic::write(
        &contents.join("Info.plist"),
        info_plist(name, &executable).as_bytes(),
    )
}

/// What a user is told this artefact does.
///
/// Phrased as the instruction it is, and matched to `Uninstall <name>` in an
/// installation, so the two halves of an application's lifecycle read the same
/// way.
fn installer_display_name(application_name: &str) -> String {
    format!("Install {}", xpack_core::safe_file_name(application_name))
}

/// The bundle's `Info.plist`.
fn info_plist(name: &str, executable: &str) -> String {
    let display = xml_escape(&installer_display_name(name));
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>CFBundleName</key>\n\t<string>{display}</string>\n\
         \t<key>CFBundleExecutable</key>\n\t<string>{}</string>\n\
         \t<key>CFBundlePackageType</key>\n\t<string>APPL</string>\n\
         \t<key>CFBundleInfoDictionaryVersion</key>\n\t<string>6.0</string>\n\
         </dict>\n</plist>\n",
        xml_escape(executable)
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The conventional name for an installer artefact.
///
/// Windows and Linux get a stem carrying the version and the platform,
/// because a release publishes one of these per platform and they are
/// downloaded into a directory that already has other things in it. The
/// spelling differs only in the word each platform uses for the thing.
///
/// macOS does not, because a bundle's directory name *is* what Finder shows
/// the user. A name built for a downloads directory reads as debris there,
/// and the version is inside the bundle for anyone who needs it.
fn default_name(manifest: &xpack_core::Manifest, target: Os) -> String {
    if target == Os::Macos {
        return format!("{}.app", installer_display_name(&manifest.application.name));
    }

    let stem = format!(
        "{}-{}-{}",
        sanitise_stem(&manifest.application.name),
        manifest.application.version.to_directory_name(),
        manifest.platform
    );
    match target {
        // The word Windows uses, and the one a user is looking for.
        Os::Windows => format!("{stem}-Setup.exe"),
        _ => format!("{stem}-installer"),
    }
}

fn sanitise_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() { "application".to_string() } else { trimmed }
}

/// Total bytes of an installer, whether it is one file or a bundle.
fn total_size(path: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64) {
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if metadata.is_file() {
            *total += metadata.len();
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            walk(&entry.path(), total);
        }
    }
    let mut total = 0;
    walk(path, &mut total);
    total
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::io(path, e))
}

// Mirrors the Unix version's signature, which genuinely can fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
fn set_executable(path: &Path) -> Result<()> {
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_artefact_is_named_for_its_platform() {
        assert_eq!(sanitise_stem("My App"), "My-App");
        assert_eq!(sanitise_stem("///"), "application");
    }

    #[test]
    fn a_bundle_name_cannot_escape_its_directory() {
        // The bundle's directory name is publisher-controlled, so a separator
        // in it would put the artefact somewhere the build never named.
        for hostile in ["Acme/Evil", ".hidden", "../../x"] {
            let name = installer_display_name(hostile);
            assert_eq!(std::path::Path::new(&name).components().count(), 1, "{name}");
            assert!(!name.contains('/'), "{name}");
        }
        assert_eq!(installer_display_name("Acme/Evil"), "Install Acme-Evil");
    }

    #[test]
    fn the_bundle_is_named_as_the_instruction_it_is() {
        // Mirrors `Uninstall <name>` in an installation: the two halves of an
        // application's lifecycle should read the same way.
        assert_eq!(installer_display_name("My App"), "Install My App");
        assert_eq!(default_name(&manifest_named("My App"), Os::Macos), "Install My App.app");
    }

    #[test]
    fn a_downloaded_artefact_says_which_release_and_platform_it_is() {
        // Five of these land in one downloads directory per release.
        let manifest = manifest_named("My App");
        assert_eq!(
            default_name(&manifest, Os::Windows),
            format!("My-App-1.2.0-{}-Setup.exe", manifest.platform)
        );
        assert_eq!(
            default_name(&manifest, Os::Linux),
            format!("My-App-1.2.0-{}-installer", manifest.platform)
        );
    }

    #[test]
    fn the_plist_escapes_a_name_that_would_break_the_xml() {
        // macOS does not report a malformed Info.plist; the bundle simply
        // never opens.
        //
        // An ampersand is the case that reaches here: it is legal in a
        // filename, so sanitising the name leaves it in place and the XML
        // escaping is the only thing standing between it and a bundle that
        // will not launch. Angle brackets never arrive, because a filename
        // cannot carry them.
        let plist = info_plist("Tom & Jerry <beta>", "app");
        assert!(plist.contains("Tom &amp; Jerry"), "{plist}");
        assert!(!plist.contains("<beta>"), "{plist}");

        // And the name in the plist is the name of the directory, which is
        // what Finder shows: the two disagreeing is a bundle that opens under
        // one name and is listed under another.
        assert!(plist.contains("Install Tom &amp; Jerry"), "{plist}");
    }

    fn manifest_named(name: &str) -> xpack_core::Manifest {
        use std::collections::BTreeMap;
        use xpack_core::manifest::{
            Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec,
        };

        xpack_core::Manifest {
            format_version: FormatVersion::CURRENT,
            application: Application {
                id: "com.example.app".into(),
                name: name.into(),
                version: xpack_core::Version::parse("1.2.0").unwrap(),
                description: None,
                publisher: None,
            },
            platform: xpack_core::Platform::host().unwrap(),
            launch: LaunchSpec {
                executable: "bin/app".into(),
                arguments: vec![],
                working_directory: None,
                environment: BTreeMap::new(),
            },
            update: UpdateSpec::default(),
            health: xpack_core::HealthSpec::default(),
            signing_key: None,
            desktop: xpack_core::DesktopSpec::default(),
            payload: PayloadSpec::default(),
            created_at: None,
        }
    }
}
