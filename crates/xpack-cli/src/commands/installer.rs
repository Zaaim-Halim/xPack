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

    /// Deprecated: the icon now comes from the package's `desktop.icon`.
    ///
    /// Still honoured, and still preferred when given, so existing builds keep
    /// working; it prints a note saying so. Only the Windows executables use
    /// it, as before.
    #[arg(long, value_name = "FILE")]
    icon: Option<PathBuf>,

    /// Settings for the installation wizard, as a JSON file.
    ///
    /// Which pages appear, the licence to show, and a few lines of wording.
    /// Every setting has a default, and so does the file: without it the
    /// wizard is the recommended one. Branding is not here; it comes from
    /// the signed package.
    #[arg(long, value_name = "FILE")]
    ui: Option<PathBuf>,

    /// For a Windows target, build on the console installer instead of the
    /// windowed one.
    ///
    /// The windowed build is the default: it is what a person double-clicks,
    /// and it opens the installation wizard with no console behind it. A shell
    /// does not wait for a windowed program, though, so an installer that is
    /// only ever run by scripts is better built on the console one. Other
    /// targets have one build and ignore this.
    #[arg(long)]
    console: bool,

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

    let (ui, licence) = load_ui(args.ui.as_deref())?;
    let plan = InstallPlan {
        format_version: InstallPlan::format_for(ui.as_ref()),
        application_id: manifest.application.id.clone(),
        application_name: manifest.application.name.clone(),
        version: manifest.application.version.to_string(),
        signing_key: signing_key.clone(),
        activate: !args.no_activate,
        ui,
    };

    let stub = match &args.stub {
        Some(path) => path.clone(),
        None => super::sibling_binary_required(stub_name(manifest.platform.os, args.console))?,
    };
    let binaries = resolve_binaries(args, manifest.platform.os)?;

    // Branded before they are embedded, and the stub before the payload is
    // appended: rewriting a resource section moves bytes, so doing it to the
    // stub afterwards would leave the trailer pointing into the wrong place.
    let workshop = tempfile::tempdir().map_err(|e| Error::io(Path::new("temporary"), e))?;
    let icon = resolve_icon(args, &manifest, &signing_key, workshop.path())?;
    let windows_icon =
        icon.as_ref().filter(|icon| icon.for_windows).map(|icon| icon.path.as_path());
    let branded = brand_for_windows(&manifest, &binaries, windows_icon, workshop.path())?;
    let binaries = branded.as_ref().unwrap_or(&binaries);

    let payload = xpack_installer::bundle::build_with_licence(
        &plan,
        &args.package,
        binaries,
        licence.as_deref(),
    )?;

    let target = manifest.platform.os;
    let output = match &args.out {
        Some(path) => path.clone(),
        None => args.out_dir.join(default_name(&manifest, target)),
    };

    let stub = match brand_stub(&manifest, &stub, windows_icon, workshop.path())? {
        Some(branded) => branded,
        None => stub,
    };

    let layout = match target {
        // Appending to a Mach-O breaks its code signature beyond repair.
        Os::Macos => {
            let icns = icon.as_ref().filter(|icon| icon.is_icns()).map(|icon| icon.path.as_path());
            let identity = BundleIdentity {
                name: &manifest.application.name,
                id: &manifest.application.id,
                version: &manifest.application.version.to_string(),
            };
            write_bundle(&output, &stub, &payload, &identity, icns)?;
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
        xpack_core::errln!();
        xpack_core::errln!(
            "note: sign and notarise this bundle before distributing it; macOS blocks an \
             unsigned downloaded installer."
        );
    } else {
        xpack_core::errln!();
        xpack_core::errln!(
            "note: sign this installer if you distribute it, and sign it *after* this step — \
             appending the payload invalidates a signature applied to the stub."
        );
    }

    super::success()
}

/// Reads the wizard settings, and the licence they name.
///
/// The licence path is relative to the settings file. In the plan written
/// into the installer it becomes the payload entry the licence travels as,
/// so the one field says where the licence is in both places.
fn load_ui(path: Option<&Path>) -> Result<(Option<xpack_installer::UiPlan>, Option<String>)> {
    let Some(path) = path else {
        return Ok((None, None));
    };
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let mut ui: xpack_installer::UiPlan = serde_json::from_str(&text)
        .map_err(|e| Error::invalid(path.display().to_string(), e.to_string()))?;
    ui.validate()?;

    let Some(relative) = &ui.license else {
        return Ok((Some(ui), None));
    };
    let file = path.parent().unwrap_or(Path::new(".")).join(relative);
    let bytes = std::fs::read(&file).map_err(|e| Error::io(&file, e))?;
    let licence = xpack_installer::bundle::check_licence(&bytes)
        .map_err(|e| Error::invalid(file.display().to_string(), e.to_string()))?
        .to_string();
    ui.license = Some(xpack_installer::bundle::LICENCE_ENTRY.to_string());
    Ok((Some(ui), Some(licence)))
}

/// The icon the installer and its executables carry.
struct ResolvedIcon {
    /// A file holding it, in the workshop or where `--icon` named.
    path: PathBuf,
    /// Whether the Windows executables can carry it.
    for_windows: bool,
}

impl ResolvedIcon {
    fn is_icns(&self) -> bool {
        self.path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("icns"))
    }
}

/// Where the icon comes from: the package's own `desktop.icon`, the one
/// source every other surface uses, unless `--icon` still names one.
///
/// Read from the package only after checking it against the key it declares,
/// with the file's size and digest held to the signed manifest. A package
/// that fails that is refused, as the installer would refuse it later. An
/// icon that is merely unusable on Windows is left off the executables with
/// a warning: a build that worked before an icon was inferred keeps working.
fn resolve_icon(
    args: &Args,
    manifest: &xpack_core::Manifest,
    signing_key: &str,
    workshop: &Path,
) -> Result<Option<ResolvedIcon>> {
    if let Some(explicit) = &args.icon {
        xpack_core::errln!(
            "note: --icon is deprecated; the installer's icon now comes from the package's \
             desktop.icon. It is still used because it was given."
        );
        return Ok(Some(ResolvedIcon { path: explicit.clone(), for_windows: true }));
    }
    let Some(name) = &manifest.desktop.icon else {
        return Ok(None);
    };

    let key = xpack_security::PublicKey::parse_hex(signing_key)?;
    let mut verified =
        PackageReader::open(&args.package)?.verify_with_keys(std::slice::from_ref(&key))?;
    let bytes = match verified.read_payload_file(name, xpack_installer::MAX_ICON_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.is_integrity_failure() => return Err(error),
        Err(error) => {
            xpack_core::errln!("warning: the package's icon is not used: {error}");
            return Ok(None);
        }
    };

    let extension = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("icon");
    let path = workshop.join(format!("application-icon.{extension}"));
    xpack_core::atomic::write(&path, &bytes)?;

    let for_windows = crate::branding::is_usable_icon(&bytes);
    if manifest.platform.os == Os::Windows && !for_windows {
        xpack_core::errln!(
            "warning: {name} is not an .ico or an image this build can read, so the Windows \
             executables carry no icon"
        );
    }
    Ok(Some(ResolvedIcon { path, for_windows }))
}

/// Gives the runtime executables the application's name and icon.
///
/// Windows only: an executable carries an icon and a display name there and
/// nowhere else, so on the other platforms this is not a gap to fill but a
/// question that does not arise.
///
/// Returns `None` when there is nothing to do, so the caller keeps using the
/// originals rather than copies of them.
fn brand_for_windows(
    manifest: &xpack_core::Manifest,
    binaries: &[PathBuf],
    icon: Option<&Path>,
    workshop: &Path,
) -> Result<Option<Vec<PathBuf>>> {
    if manifest.platform.os != Os::Windows {
        return Ok(None);
    }

    let names = xpack_core::BinaryNames::from_display_name(&manifest.application.name);
    let mut branded = Vec::with_capacity(binaries.len());

    for source in binaries {
        let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        // Named for what it will be called once installed, not for what it
        // is called in the distribution: that is the name a user sees in
        // Task Manager beside the process.
        let Some((installed, role)) = branded_identity(&names, stem) else {
            // An unknown binary is carried through unbranded rather than
            // guessed at. A wrong name on a process is worse than none.
            branded.push(source.clone());
            continue;
        };

        // Copied under the name it arrived with, never under the name it will
        // be installed as. See `branded_identity`.
        let destination = workshop.join(format!("{stem}.exe"));
        std::fs::copy(source, &destination).map_err(|e| Error::io(source, e))?;
        crate::branding::apply(
            &destination,
            &crate::branding::Branding {
                name: &manifest.application.name,
                description: format!("{} {}", manifest.application.name, role.to_lowercase()),
                // The name the file will carry once installed, which is what
                // this field is for: it describes the shipped executable, not
                // the copy sitting in a build directory.
                original_file_name: format!("{installed}.exe"),
                version: &manifest.application.version,
                publisher: manifest.application.publisher.as_deref(),
            },
            icon,
        )?;
        branded.push(destination);
    }
    Ok(Some(branded))
}

/// What a runtime binary will be called once installed, and what it does.
///
/// `None` for anything this build does not recognise.
///
/// # The file keeps its own name until it is installed
///
/// Only the *metadata* written into the copy is branded. The copy itself stays
/// under the stem it arrived with, because the stub finds each binary in the
/// payload by that stem -- it is how it tells a launcher from an updater.
/// Renaming them to the application's names here left the stub recognising
/// none of them: it installed an application with no launcher, no updater and
/// no uninstaller, reported success, and left a user with a directory they
/// could not run.
///
/// Nothing is lost by waiting. The installed name is decided at install time
/// from the application the package declares, which is the same name computed
/// here, and it is applied wherever the binary lands.
fn branded_identity(names: &xpack_core::BinaryNames, stem: &str) -> Option<(String, &'static str)> {
    match stem {
        "xpack-launcher" => Some((names.launcher(), "Console launcher")),
        "xpack-launcherw" => Some((names.windowed_launcher(), "Launcher")),
        "xpack-updater" => Some((names.updater(), "Updater")),
        "xpack-uninstaller" => Some((names.uninstaller(), "Uninstaller")),
        "xpack-notify" => Some((names.notifier(), "Update notice")),
        _ => None,
    }
}

/// Gives the installer itself the application's name and icon.
///
/// This is the executable a user downloads and double-clicks, so it is the
/// one whose icon they see before anything of the application exists.
fn brand_stub(
    manifest: &xpack_core::Manifest,
    stub: &Path,
    icon: Option<&Path>,
    workshop: &Path,
) -> Result<Option<PathBuf>> {
    if manifest.platform.os != Os::Windows {
        return Ok(None);
    }

    let display = installer_display_name(&manifest.application.name);
    let destination = workshop.join(format!("{display}.exe"));
    std::fs::copy(stub, &destination).map_err(|e| Error::io(stub, e))?;
    crate::branding::apply(
        &destination,
        &crate::branding::Branding {
            name: &manifest.application.name,
            description: display.clone(),
            original_file_name: format!("{display}.exe"),
            version: &manifest.application.version,
            publisher: manifest.application.publisher.as_deref(),
        },
        icon,
    )?;
    Ok(Some(destination))
}

/// The installer build a target gets when no stub is named.
fn stub_name(target: Os, console: bool) -> &'static str {
    if target == Os::Windows && !console { "xpack-installerw" } else { "xpack-installer" }
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
///
/// With an `.icns`, Finder shows the application's icon on the installer, as
/// it will on the installed application.
fn write_bundle(
    output: &Path,
    stub: &Path,
    payload: &[u8],
    identity: &BundleIdentity<'_>,
    icns: Option<&Path>,
) -> Result<()> {
    replace_previous_bundle(output)?;
    let name = identity.name;
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
    if let Some(icns) = icns {
        let bytes = std::fs::read(icns).map_err(|e| Error::io(icns, e))?;
        xpack_core::atomic::write(&resources.join(BUNDLE_ICON_FILE), &bytes)?;
    }
    xpack_core::atomic::write(
        &contents.join("Info.plist"),
        info_plist(identity, &executable, icns.is_some()).as_bytes(),
    )
}

/// What an installer bundle says about itself.
struct BundleIdentity<'a> {
    /// The application's display name.
    name: &'a str,
    /// The application's id, from which the installer's own is made.
    id: &'a str,
    /// The version the installer installs.
    version: &'a str,
}

/// Clears a bundle an earlier build left at `output`, so this one is written
/// afresh rather than over it.
///
/// Written over, a bundle keeps files the new build no longer has, and keeps
/// its directory's timestamp, which is what Finder goes by to decide whether
/// the icon it cached is still current: a rebuilt installer would go on
/// showing the old icon. Only a directory that is recognisably an installer
/// bundle is removed; anything else at that path is refused, not deleted.
fn replace_previous_bundle(output: &Path) -> Result<()> {
    if !output.exists() {
        return Ok(());
    }
    let payload = output.join("Contents").join("Resources").join(SIDECAR_NAME);
    if !payload.is_file() {
        return Err(Error::invalid(
            "installer",
            format!(
                "{} already exists and is not an installer bundle; refusing to replace it",
                output.display()
            ),
        ));
    }
    std::fs::remove_dir_all(output).map_err(|e| Error::io(output, e))
}

/// What a user is told this artefact does.
///
/// Phrased as the instruction it is, and matched to `Uninstall <name>` in an
/// installation, so the two halves of an application's lifecycle read the same
/// way.
fn installer_display_name(application_name: &str) -> String {
    format!("Install {}", xpack_core::safe_file_name(application_name))
}

/// The icon's file name inside an installer bundle's `Resources`.
const BUNDLE_ICON_FILE: &str = "AppIcon.icns";

/// The bundle's `Info.plist`.
///
/// The identifier is the application's own with `.installer` appended: the
/// installer is a different program from the application, and Launch Services
/// keys what it knows about a bundle — its icon above all — by identifier.
/// High resolution is declared because a bundle that does not say so may be
/// run in low-resolution mode, which blurs every word of the wizard on a
/// Retina display.
fn info_plist(identity: &BundleIdentity<'_>, executable: &str, has_icon: bool) -> String {
    let display = xml_escape(&installer_display_name(identity.name));
    let id = xml_escape(&format!("{}.installer", identity.id));
    let version = xml_escape(identity.version);
    let icon = if has_icon {
        format!("\t<key>CFBundleIconFile</key>\n\t<string>{BUNDLE_ICON_FILE}</string>\n")
    } else {
        String::new()
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>CFBundleName</key>\n\t<string>{display}</string>\n\
         \t<key>CFBundleDisplayName</key>\n\t<string>{display}</string>\n\
         \t<key>CFBundleIdentifier</key>\n\t<string>{id}</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n\
         \t<key>CFBundleVersion</key>\n\t<string>{version}</string>\n\
         \t<key>NSHighResolutionCapable</key>\n\t<true/>\n\
         \t<key>CFBundleExecutable</key>\n\t<string>{}</string>\n\
         \t<key>CFBundlePackageType</key>\n\t<string>APPL</string>\n\
         \t<key>CFBundleInfoDictionaryVersion</key>\n\t<string>6.0</string>\n\
         {icon}</dict>\n</plist>\n",
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
    fn every_runtime_binary_the_installer_ships_is_recognised() {
        // The stub tells a launcher from an updater by the stem of the file it
        // finds in the payload. A binary this table does not know is shipped
        // unbranded, which is survivable; one that is branded *and renamed* is
        // not, because the stub then recognises nothing and installs an
        // application with no launcher at all.
        let names = xpack_core::BinaryNames::from_display_name("Demo App");
        for stem in [
            "xpack-launcher",
            "xpack-launcherw",
            "xpack-updater",
            "xpack-uninstaller",
            "xpack-notify",
        ] {
            let (installed, role) =
                branded_identity(&names, stem).unwrap_or_else(|| panic!("{stem} is unrecognised"));
            assert!(installed.contains("Demo App"), "{stem} -> {installed}");
            assert!(!role.is_empty());
        }
        assert_eq!(branded_identity(&names, "something-else"), None);
    }

    #[test]
    fn the_installed_name_is_never_the_name_the_payload_carries() {
        // The two are deliberately different: the payload keeps xPack's stems
        // so the stub can identify each binary, and the branded name is what
        // it is called once installed.
        let names = xpack_core::BinaryNames::from_display_name("Demo App");
        for stem in ["xpack-launcher", "xpack-launcherw", "xpack-updater", "xpack-uninstaller"] {
            let (installed, _) = branded_identity(&names, stem).expect("a known binary");
            assert_ne!(installed, stem, "the payload name and the installed name collapsed");
        }
    }

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
        let identity =
            BundleIdentity { name: "Tom & Jerry <beta>", id: "com.example.tj", version: "1.0.0" };
        let plist = info_plist(&identity, "app", false);
        assert!(plist.contains("Tom &amp; Jerry"), "{plist}");
        assert!(!plist.contains("<beta>"), "{plist}");

        // And the name in the plist is the name of the directory, which is
        // what Finder shows: the two disagreeing is a bundle that opens under
        // one name and is listed under another.
        assert!(plist.contains("Install Tom &amp; Jerry"), "{plist}");
    }

    #[test]
    fn a_windows_installer_is_built_on_the_windowed_stub_unless_scripts_need_the_console() {
        assert_eq!(stub_name(Os::Windows, false), "xpack-installerw");
        assert_eq!(stub_name(Os::Windows, true), "xpack-installer");
        // One build elsewhere, whatever was asked.
        assert_eq!(stub_name(Os::Macos, false), "xpack-installer");
        assert_eq!(stub_name(Os::Linux, true), "xpack-installer");
    }

    #[test]
    fn the_plist_names_the_icon_only_when_there_is_one() {
        let identity = BundleIdentity { name: "App", id: "com.example.app", version: "1.2.3" };
        assert!(info_plist(&identity, "app", true).contains("<string>AppIcon.icns</string>"));
        assert!(!info_plist(&identity, "app", false).contains("CFBundleIconFile"));
    }

    #[test]
    fn the_plist_gives_the_installer_its_own_identity_and_high_resolution() {
        let identity = BundleIdentity { name: "App", id: "com.example.app", version: "1.2.3" };
        let plist = info_plist(&identity, "app", false);
        assert!(
            plist.contains(
                "<key>CFBundleIdentifier</key>\n\t<string>com.example.app.installer</string>"
            ),
            "{plist}"
        );
        assert!(
            plist.contains("<key>CFBundleShortVersionString</key>\n\t<string>1.2.3</string>"),
            "{plist}"
        );
        assert!(plist.contains("<key>NSHighResolutionCapable</key>\n\t<true/>"), "{plist}");
    }

    #[test]
    fn a_rebuilt_bundle_replaces_the_old_one_rather_than_writing_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("Install App.app");
        let stub = dir.path().join("stub");
        std::fs::write(&stub, b"stub").unwrap();
        let icns = dir.path().join("icon.icns");
        std::fs::write(&icns, b"icon").unwrap();
        let identity = BundleIdentity { name: "App", id: "com.example.app", version: "1.0.0" };

        write_bundle(&output, &stub, b"payload", &identity, Some(&icns)).unwrap();
        // The next build has no icon; the old one must not survive it.
        write_bundle(&output, &stub, b"payload", &identity, None).unwrap();

        assert!(!output.join("Contents/Resources").join(BUNDLE_ICON_FILE).exists());
        assert!(output.join("Contents/Resources").join(SIDECAR_NAME).is_file());
    }

    #[test]
    fn something_else_at_the_output_path_is_refused_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("Install App.app");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("precious.txt"), b"keep me").unwrap();
        let stub = dir.path().join("stub");
        std::fs::write(&stub, b"stub").unwrap();
        let identity = BundleIdentity { name: "App", id: "com.example.app", version: "1.0.0" };

        assert!(write_bundle(&output, &stub, b"payload", &identity, None).is_err());
        assert!(output.join("precious.txt").is_file(), "an unrelated directory was deleted");
    }

    /// Writes a settings file, and a licence beside it, into `dir`.
    fn settings(dir: &Path, json: &str, licence: Option<&[u8]>) -> PathBuf {
        let path = dir.join("ui.json");
        std::fs::write(&path, json).unwrap();
        if let Some(bytes) = licence {
            std::fs::write(dir.join("LICENSE.txt"), bytes).unwrap();
        }
        path
    }

    #[test]
    fn no_settings_file_means_the_recommended_wizard() {
        assert_eq!(load_ui(None).unwrap(), (None, None));
    }

    #[test]
    fn a_licence_is_read_beside_the_settings_file_and_carried_in_the_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings(dir.path(), r#"{"license": "LICENSE.txt"}"#, Some(b"Terms."));

        let (ui, licence) = load_ui(Some(&path)).unwrap();
        assert_eq!(licence.as_deref(), Some("Terms."));
        assert_eq!(
            ui.unwrap().license.as_deref(),
            Some(xpack_installer::bundle::LICENCE_ENTRY),
            "the plan points at where the licence travels"
        );
    }

    #[test]
    fn a_settings_file_that_is_wrong_stops_the_build() {
        let dir = tempfile::tempdir().unwrap();
        for (json, licence) in [
            // Misspelt: silently ignoring it would ship the default.
            (r#"{"launchOnFinnish": true}"#, None),
            // Branding has one source, the signed package.
            (r#"{"logo": "logo.png"}"#, None),
            // A placeholder that is not always available.
            (r#"{"text": {"welcome": "By {publisher}"}}"#, None),
            // A licence that is not there, or is empty, or is not text.
            (r#"{"license": "missing.txt"}"#, None),
            (r#"{"license": "LICENSE.txt"}"#, Some(&b"  \n"[..])),
            (r#"{"license": "LICENSE.txt"}"#, Some(&[0xff, 0xfe, 0x00][..])),
        ] {
            let path = settings(dir.path(), json, licence);
            assert!(load_ui(Some(&path)).is_err(), "{json} was accepted");
        }
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
                keep_working_directory: false,
                environment: BTreeMap::new(),
            },
            update: UpdateSpec::default(),
            health: xpack_core::HealthSpec::default(),
            signing_key: None,
            desktop: xpack_core::DesktopSpec::default(),
            payload: PayloadSpec::default(),
            created_at: None,
            command: None,
            commands: Vec::new(),
        }
    }
}
