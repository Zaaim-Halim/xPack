//! Making a Windows executable look like the application it serves.
//!
//! On macOS and Linux an executable carries no icon and no display name. The
//! `.app` bundle and the `.desktop` file supply both, and xPack already
//! writes them. Windows is the exception: the icon Explorer draws and the
//! name Task Manager shows live inside the binary, in its resource section.
//!
//! So this exists for one platform, and is the whole of what "the programs
//! carry the application's name and icon" means there.
//!
//! # Why this runs on the publisher's machine
//!
//! Rewriting a resource section changes the bytes of an executable, which
//! means it must happen **before** the publisher signs anything — a
//! signature applied first would no longer cover the file. Doing it here, at
//! the moment an installer is assembled, puts it in the same window as the
//! existing rule that appending a payload invalidates a signature.
//!
//! It also keeps a PE editor out of the binaries that ship. Nothing a user
//! runs contains any of this.

use std::path::Path;

use editpe::types::{VersionU16, VersionU32};
use editpe::{Image, ResourceDirectory, VersionInfo, VersionStringTable};
use xpack_core::{Error, Result, Version};

/// The language and codepage a version resource is filed under.
///
/// US English, Unicode. Windows looks the strings up by this key, and one
/// that names a language the resource does not contain reads as absent, so a
/// single table under the conventional key is more reliable than several.
const LANGUAGE: &str = "040904b0";
const LANGUAGE_ID: u16 = 0x0409;
const CODEPAGE: u16 = 0x04b0;

/// The first four bytes of an `.ico` file.
const ICO_MAGIC: [u8; 4] = [0x00, 0x00, 0x01, 0x00];

/// What an executable should say about itself.
pub(crate) struct Branding<'a> {
    /// The application's display name.
    pub name: &'a str,
    /// What this particular executable is, as a person would read it.
    ///
    /// Shown by Task Manager, which is where a user meets a background
    /// process they did not expect and decides whether to end it.
    pub description: String,
    /// The file's own name, recorded so a renamed copy can be recognised.
    pub original_file_name: String,
    pub version: &'a Version,
    pub publisher: Option<&'a str>,
}

/// Whether these bytes are an icon a Windows executable can carry.
///
/// An `.ico` as it is, or any image this build can decode. Asked of an icon
/// that was not handed over explicitly, so a package whose icon suits another
/// platform still builds, only without one.
pub(crate) fn is_usable_icon(bytes: &[u8]) -> bool {
    (bytes.len() >= 4 && bytes[..4] == ICO_MAGIC) || image::load_from_memory(bytes).is_ok()
}

/// The application manifest every branded executable carries.
///
/// Three things Windows otherwise decides for itself, each wrongly for xPack:
///
/// * **`asInvoker`.** Without a declared execution level, Windows guesses
///   from the file name whether a program is an installer, and names with
///   "Setup", "Install" or "Update" in them — which every one of these has —
///   are guessed to need administrator rights. The guess applies to 32-bit
///   programs only, but it must never be made: nothing xPack runs is elevated.
/// * **System DPI awareness.** Without it Windows draws a window at 96 DPI
///   and stretches the bitmap, which blurs every word on a scaled display.
///   System-aware rather than per-monitor, because the windows these programs
///   draw are laid out once, at the DPI they start with.
/// * **Common Controls 6.** The current look of buttons and checkboxes, rather
///   than the one from Windows 95.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;

/// Rewrites an executable's icon, version information and manifest in place.
///
/// The file is read, rewritten in memory and written back atomically, so an
/// interrupted run cannot leave a half-edited executable behind.
pub(crate) fn apply(binary: &Path, branding: &Branding, icon: Option<&Path>) -> Result<()> {
    let mut image = Image::parse_file(binary).map_err(|e| {
        Error::invalid("executable", format!("{} is not a PE: {e}", binary.display()))
    })?;

    let mut resources = image.resource_directory().cloned().unwrap_or_default();

    if let Some(icon) = icon {
        set_icon(&mut resources, icon)?;
    }
    resources
        .set_version_info(&version_info(branding))
        .map_err(|e| Error::invalid("executable", format!("version information: {e}")))?;
    resources
        .set_manifest(MANIFEST)
        .map_err(|e| Error::invalid("executable", format!("manifest: {e}")))?;

    image
        .set_resource_directory(resources)
        .map_err(|e| Error::invalid("executable", format!("resource section: {e}")))?;

    let mut bytes = Vec::new();
    image.write_writer(&mut bytes).map_err(|e| {
        Error::invalid("executable", format!("rewriting {}: {e}", binary.display()))
    })?;

    xpack_core::atomic::write(binary, &bytes)
}

/// Loads an icon, from either of the two formats a publisher is likely to have.
///
/// An `.ico` already holds the several sizes Windows picks between, so it is
/// passed through untouched. A `.png` is a single image, and the sizes are
/// generated from it — which is what lets one icon file serve both this and
/// the `.desktop` entry Linux reads, rather than making a publisher keep two.
fn set_icon(resources: &mut ResourceDirectory, icon: &Path) -> Result<()> {
    let bytes = std::fs::read(icon).map_err(|e| Error::io(icon, e))?;

    if bytes.len() >= 4 && bytes[..4] == ICO_MAGIC {
        return resources
            .set_main_icon(bytes.as_slice())
            .map_err(|e| Error::invalid("icon", format!("{}: {e}", icon.display())));
    }

    let decoded = image::load_from_memory(&bytes).map_err(|e| {
        Error::invalid(
            "icon",
            format!("{} is neither an .ico nor an image this build can read: {e}", icon.display()),
        )
    })?;
    resources
        .set_main_icon(&decoded)
        .map_err(|e| Error::invalid("icon", format!("{}: {e}", icon.display())))
}

/// Builds the version resource Explorer and Task Manager read.
fn version_info(branding: &Branding) -> VersionInfo {
    let mut info = VersionInfo::default();

    let packed = packed_version(branding.version);
    info.info.file_version = packed;
    info.info.product_version = packed;

    let mut strings =
        VersionStringTable { key: LANGUAGE.to_string(), ..VersionStringTable::default() };
    let version = branding.version.to_string();
    let mut put = |key: &str, value: &str| {
        strings.strings.insert(key.to_string(), value.to_string());
    };
    // FileDescription is the one that matters most: it is the column Task
    // Manager shows, so it is what a user reads when deciding whether a
    // process is theirs.
    put("FileDescription", &branding.description);
    put("ProductName", branding.name);
    put("InternalName", branding.name);
    put("OriginalFilename", &branding.original_file_name);
    put("FileVersion", &version);
    put("ProductVersion", &version);
    if let Some(publisher) = branding.publisher {
        put("CompanyName", publisher);
    }

    info.strings = vec![strings];
    // The translation table has to agree with the string table's key, or
    // Windows looks for strings in a language the resource does not carry and
    // reports the version information as missing.
    info.vars = vec![VersionU16 { major: LANGUAGE_ID, minor: CODEPAGE }];
    info
}

/// Packs a semantic version into the two words Windows stores it in.
///
/// `FILEVERSION` is four 16-bit parts. A semantic version has three, so the
/// fourth is zero; a component beyond what 16 bits hold is clamped rather
/// than wrapped, because a version silently reading as 1.0.0 would be worse
/// than one reading as 1.0.65535.
fn packed_version(version: &Version) -> VersionU32 {
    let semver = version.as_semver();
    let part = |value: u64| u32::try_from(value.min(u64::from(u16::MAX))).unwrap_or(0);
    VersionU32 {
        major: (part(semver.major) << 16) | part(semver.minor),
        minor: part(semver.patch) << 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn branding<'a>(name: &'a str, version: &'a Version) -> Branding<'a> {
        Branding {
            name,
            description: format!("{name} Updater"),
            original_file_name: format!("{name} Updater.exe"),
            version,
            publisher: Some("Example Ltd"),
        }
    }

    #[test]
    fn the_version_is_packed_the_way_windows_reads_it() {
        let packed = packed_version(&version("1.2.3"));
        assert_eq!(packed.major, (1 << 16) | 2);
        assert_eq!(packed.minor, 3 << 16);
    }

    #[test]
    fn a_component_too_large_for_the_field_is_clamped_rather_than_wrapped() {
        // Wrapping would turn 1.0.65536 into 1.0.0, which reads as a
        // completely different release rather than an inaccurate one.
        let packed = packed_version(&version("1.0.70000"));
        assert_eq!(packed.minor >> 16, u32::from(u16::MAX));
    }

    #[test]
    fn task_manager_is_told_what_this_particular_executable_is() {
        let v = version("1.2.3");
        let info = version_info(&branding("My App", &v));
        let strings = &info.strings[0].strings;

        assert_eq!(strings.get("FileDescription").unwrap(), "My App Updater");
        assert_eq!(strings.get("ProductName").unwrap(), "My App");
        assert_eq!(strings.get("FileVersion").unwrap(), "1.2.3");
        assert_eq!(strings.get("CompanyName").unwrap(), "Example Ltd");
    }

    #[test]
    fn the_translation_table_agrees_with_the_strings_it_describes() {
        // Disagreeing makes Windows look for the strings under a language the
        // resource does not carry, and report no version information at all.
        let v = version("1.0.0");
        let info = version_info(&branding("My App", &v));

        assert_eq!(info.strings[0].key, LANGUAGE);
        assert_eq!(info.vars, vec![VersionU16 { major: LANGUAGE_ID, minor: CODEPAGE }]);
    }

    #[test]
    fn the_manifest_never_asks_for_elevation_and_declares_system_dpi_awareness() {
        assert!(MANIFEST.contains(r#"<requestedExecutionLevel level="asInvoker""#));
        assert!(!MANIFEST.contains("requireAdministrator"));
        assert!(!MANIFEST.contains("highestAvailable"));
        assert!(MANIFEST.contains(">true</dpiAware>"));
        // Per-monitor would promise a relayout on every monitor change that
        // these windows do not do.
        assert!(!MANIFEST.contains("PerMonitor"));
        assert!(MANIFEST.contains(r#"name="Microsoft.Windows.Common-Controls" version="6.0.0.0""#));
    }

    #[test]
    fn the_manifest_is_well_formed_xml_windows_will_load() {
        // A manifest Windows cannot parse stops the program from starting at
        // all, with a side-by-side configuration error. Every element opened
        // is closed, and in order.
        let mut open: Vec<String> = Vec::new();
        let mut rest = MANIFEST;
        while let Some(start) = rest.find('<') {
            let end = rest[start..].find('>').expect("a closed tag") + start;
            let tag = &rest[start + 1..end];
            rest = &rest[end + 1..];
            if tag.starts_with('?') || tag.ends_with('/') {
                continue;
            }
            let name = tag.trim_start_matches('/').split_whitespace().next().unwrap();
            if tag.starts_with('/') {
                assert_eq!(open.pop().as_deref(), Some(name), "</{name}> closes the wrong element");
            } else {
                open.push(name.to_string());
            }
        }
        assert!(open.is_empty(), "left open: {open:?}");
    }

    #[test]
    fn an_absent_publisher_leaves_the_field_out_rather_than_writing_nothing() {
        let v = version("1.0.0");
        let mut without = branding("My App", &v);
        without.publisher = None;

        assert!(!version_info(&without).strings[0].strings.contains_key("CompanyName"));
    }
}
