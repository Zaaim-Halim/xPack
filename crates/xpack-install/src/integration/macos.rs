//! macOS application bundles.
//!
//! macOS has no menu file and no shortcut format. What it has is the bundle: a
//! directory ending in `.app` with a known shape, which Finder, Spotlight, the
//! Dock and Launch Services all treat as an application. So the entry created
//! here is a small bundle in `~/Applications` that starts the real launcher.
//!
//! ```text
//! ~/Applications/Example.app/
//! └── Contents/
//!     ├── Info.plist          identity, so Launch Services can index it
//!     ├── MacOS/Example       the trampoline described below
//!     └── Resources/          the icon, when the package ships one
//! ```
//!
//! # Why a trampoline and not a copy of the launcher
//!
//! The obvious bundle contains a copy of `xpack-launcher`. It cannot: the
//! launcher works out which application it belongs to by resolving its own
//! path and treating the containing directory as the installation root. A copy
//! inside `Contents/MacOS` would decide that `Contents/MacOS` *is* the
//! installation, find no state file there, and fail.
//!
//! `XPACK_APPLICATION_DIR` would override that, but baking an absolute path
//! into an environment variable inside a bundle means the bundle and the
//! installation can drift apart silently.
//!
//! So `Contents/MacOS/<name>` is a two-line shell script that executes the
//! launcher where it actually lives. macOS is happy to run a script as a
//! bundle executable, it costs one `exec`, and the launcher keeps its single
//! way of locating itself.
//!
//! # What this bundle is not
//!
//! It is not code-signed and it is not notarised. Neither is possible here:
//! signing needs a Developer ID certificate that belongs to the publisher, not
//! to xPack, and notarisation needs Apple to see the artefact. A bundle the
//! user creates locally by installing a package is not quarantined — the
//! quarantine attribute is applied by whatever *downloaded* a file, and these
//! files are written by this process — so Gatekeeper does not block it. A
//! publisher shipping a signed application still signs their own payload; this
//! wrapper does not participate in that seal, which is exactly why it wraps
//! rather than contains.

use std::path::{Path, PathBuf};

use super::{Entry, Outcome, Roots, remove_paths};

/// Creates the bundle in `~/Applications`.
pub(super) fn install(entry: &Entry, roots: &Roots) -> Outcome {
    let bundle = bundle_path(entry, roots);

    let executable_name = bundle_executable_name(&entry.name);
    let contents = bundle.join("Contents");
    let macos_dir = contents.join("MacOS");
    let resources = contents.join("Resources");

    for dir in [&macos_dir, &resources] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return Outcome::Failed(format!("{}: {e}", dir.display()));
        }
    }

    // The icon is copied in rather than referenced: `CFBundleIconFile` names a
    // file inside `Contents/Resources` and cannot point outside the bundle.
    let icon_file = match &entry.icon {
        Some(source) => copy_icon(source, &resources),
        None => None,
    };

    let plist = render_info_plist(entry, &executable_name, icon_file.as_deref());
    if let Err(e) = xpack_core::atomic::write(&contents.join("Info.plist"), plist.as_bytes()) {
        return Outcome::Failed(e.to_string());
    }

    let trampoline = macos_dir.join(&executable_name);
    if let Err(e) =
        xpack_core::atomic::write(&trampoline, render_trampoline(&entry.target).as_bytes())
    {
        return Outcome::Failed(e.to_string());
    }
    if let Err(e) = set_executable(&trampoline) {
        return Outcome::Failed(e.to_string());
    }

    Outcome::Done(vec![bundle])
}

/// Removes the bundle.
pub(super) fn remove(entry: &Entry, roots: &Roots) -> Outcome {
    remove_paths(vec![bundle_path(entry, roots)])
}

/// `~/Applications/<Name>.app`.
///
/// The per-user directory, not `/Applications`, so that installing needs no
/// administrator password — the same choice the installation root makes.
fn bundle_path(entry: &Entry, roots: &Roots) -> PathBuf {
    roots.home.join("Applications").join(format!("{}.app", bundle_directory_name(&entry.name)))
}

/// Makes a display name safe to use as a directory name.
///
/// A display name is free-form Unicode chosen by a publisher. A `/` in one
/// would create a nested directory somewhere unintended, and a leading `.`
/// would hide the bundle from Finder entirely. A colon is remapped because
/// Finder still presents it as a path separator.
pub(super) fn bundle_directory_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == ':' || c.is_control() { '-' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() { "Application".to_string() } else { trimmed.to_string() }
}

/// The name of the file inside `Contents/MacOS`.
///
/// `CFBundleExecutable` is looked up by exact name, and a space in it works,
/// but the value has to match the file. Derived from the same sanitised name
/// so the two can never disagree.
pub(super) fn bundle_executable_name(name: &str) -> String {
    bundle_directory_name(name)
}

/// The script that starts the real launcher.
///
/// `exec` replaces the script process rather than leaving a shell waiting on
/// a child, which matters: macOS watches the bundle's executable to decide
/// whether the application is still running, and a lingering `sh` would keep
/// the bounce animation going after the real process had gone.
pub(super) fn render_trampoline(target: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         # Generated by xPack. Starts the launcher where it is installed, so\n\
         # that the launcher can resolve its own installation from its path.\n\
         exec {} \"$@\"\n",
        shell_quote(&target.to_string_lossy())
    )
}

/// The launcher the `~/Applications` bundle for `name` starts, if there is one.
pub(super) fn recorded_target(name: &str, roots: &Roots) -> Option<PathBuf> {
    let bundle =
        roots.home.join("Applications").join(format!("{}.app", bundle_directory_name(name)));
    let script = bundle.join("Contents").join("MacOS").join(bundle_executable_name(name));
    parse_trampoline(&std::fs::read_to_string(script).ok()?)
}

/// Reads back the path [`render_trampoline`] wrote, and nothing else.
///
/// Only the exact line xPack writes is accepted. A script someone edited by
/// hand says nothing reliable about where an installation is.
fn parse_trampoline(script: &str) -> Option<PathBuf> {
    let line = script.lines().find_map(|line| line.strip_prefix("exec '"))?;
    let quoted = line.strip_suffix(" \"$@\"")?.strip_suffix('\'')?;
    Some(PathBuf::from(quoted.replace(r"'\''", "'")))
}

/// Single-quotes a path for `/bin/sh`.
///
/// An installation root contains an application id and sits under a home
/// directory, either of which can contain a space. Single quotes suppress
/// every expansion `sh` would otherwise perform, with the usual escape for an
/// embedded quote.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Renders `Info.plist`.
pub(super) fn render_info_plist(
    entry: &Entry,
    executable: &str,
    icon_file: Option<&str>,
) -> String {
    let mut body = String::new();
    body.push_str(&pair("CFBundleName", &entry.name));
    body.push_str(&pair("CFBundleDisplayName", &entry.name));
    body.push_str(&pair("CFBundleIdentifier", &entry.application_id));
    body.push_str(&pair("CFBundleExecutable", executable));
    body.push_str(&pair("CFBundleVersion", &entry.version.to_string()));
    body.push_str(&pair("CFBundleShortVersionString", &entry.version.to_string()));
    body.push_str(&pair("CFBundlePackageType", "APPL"));
    body.push_str(&pair("CFBundleInfoDictionaryVersion", "6.0"));
    if let Some(icon) = icon_file {
        body.push_str(&pair("CFBundleIconFile", icon));
    }
    // Without this a bundle whose executable writes to stdout is treated as a
    // background agent by some tooling. The application is a normal one.
    body.push_str("\t<key>LSBackgroundOnly</key>\n\t<false/>\n");

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
    )
}

fn pair(key: &str, value: &str) -> String {
    format!("\t<key>{}</key>\n\t<string>{}</string>\n", xml_escape(key), xml_escape(value))
}

/// Escapes text for an XML property list.
///
/// A publisher's display name reaches this unaltered. An unescaped `<` would
/// make the plist malformed, and macOS silently refuses to index a bundle
/// whose `Info.plist` does not parse — the application would simply never
/// appear, with nothing said about why.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Copies the icon into `Contents/Resources`, returning its file name.
fn copy_icon(source: &Path, resources: &Path) -> Option<String> {
    let name = source.file_name()?.to_str()?.to_string();
    match std::fs::read(source) {
        Ok(bytes) => match xpack_core::atomic::write(&resources.join(&name), &bytes) {
            Ok(()) => Some(name),
            Err(error) => {
                tracing::warn!(%error, "could not place the bundle icon");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, path = %source.display(), "could not read the bundle icon");
            None
        }
    }
}

fn set_executable(path: &Path) -> xpack_core::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| xpack_core::Error::io(path, e))
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_path_a_trampoline_starts_reads_back_exactly() {
        for target in [
            "/Users/me/Applications/xPack/com.example.app/Example",
            "/Users/My Name/it's here/Example",
        ] {
            let script = render_trampoline(Path::new(target));
            assert_eq!(parse_trampoline(&script), Some(PathBuf::from(target)), "{script}");
        }
    }

    #[test]
    fn a_script_xpack_did_not_write_says_nothing() {
        assert_eq!(parse_trampoline("#!/bin/sh\nexec /somewhere/else\n"), None);
        assert_eq!(parse_trampoline(""), None);
    }
    use super::*;

    fn entry() -> Entry {
        Entry {
            application_id: "com.example.app".into(),
            name: "Example App".into(),
            description: Some("Does a thing".into()),
            publisher: Some("Example Ltd".into()),
            version: xpack_core::Version::parse("1.2.3").unwrap(),
            root: PathBuf::from("/Users/u/Library/Application Support/xpack/com.example.app"),
            target: PathBuf::from(
                "/Users/u/Library/Application Support/xpack/com.example.app/xpack-launcher",
            ),
            icon: None,
            categories: Vec::new(),
            terminal: false,
            uninstaller: None,
        }
    }

    #[test]
    fn the_trampoline_quotes_a_path_containing_spaces() {
        // The default install root on macOS is under "Application Support",
        // so this is the normal case rather than an unusual one.
        let script = render_trampoline(&entry().target);
        assert!(script.contains("exec '/Users/u/Library/Application Support/xpack/"), "{script}");
    }

    #[test]
    fn the_trampoline_replaces_itself_rather_than_waiting() {
        // A lingering `sh` would keep macOS believing the application is
        // still starting after the real process has gone.
        assert!(render_trampoline(Path::new("/x/launcher")).contains("exec "));
    }

    #[test]
    fn a_quote_in_a_path_cannot_break_out_of_the_trampoline() {
        let script = render_trampoline(Path::new("/tmp/it's/launcher"));
        assert!(script.contains(r"'/tmp/it'\''s/launcher'"), "{script}");
    }

    #[test]
    fn the_plist_names_the_executable_that_is_actually_written() {
        let e = entry();
        let name = bundle_executable_name(&e.name);
        let plist = render_info_plist(&e, &name, None);
        assert!(plist.contains(&format!("<string>{name}</string>")), "{plist}");
        assert!(plist.contains("<key>CFBundleExecutable</key>"));
    }

    #[test]
    fn the_plist_escapes_a_name_that_would_otherwise_break_the_xml() {
        // macOS does not report a malformed Info.plist. The application simply
        // never appears.
        let mut e = entry();
        e.name = "Tom & Jerry <beta>".into();
        let plist = render_info_plist(&e, "app", None);
        assert!(plist.contains("Tom &amp; Jerry &lt;beta&gt;"), "{plist}");
        assert!(!plist.contains("<beta>"));
    }

    #[test]
    fn a_slash_in_a_display_name_cannot_create_a_nested_directory() {
        assert_eq!(bundle_directory_name("Acme/Evil"), "Acme-Evil");
    }

    #[test]
    fn a_leading_dot_cannot_hide_the_bundle_from_finder() {
        assert_eq!(bundle_directory_name(".hidden"), "hidden");
    }

    #[test]
    fn a_name_that_sanitises_to_nothing_still_produces_a_bundle() {
        assert_eq!(bundle_directory_name("   "), "Application");
        assert_eq!(bundle_directory_name("."), "Application");
    }

    #[test]
    fn the_icon_key_is_absent_when_no_icon_was_copied() {
        assert!(!render_info_plist(&entry(), "app", None).contains("CFBundleIconFile"));
        assert!(render_info_plist(&entry(), "app", Some("icon.icns")).contains("CFBundleIconFile"));
    }

    fn roots(base: &Path) -> Roots {
        Roots { data: base.join("data"), home: base.join("home") }
    }

    #[test]
    fn the_bundle_lives_under_the_users_own_applications_directory() {
        // Not /Applications: writing there needs an administrator password,
        // and a per-user installation must never ask for one.
        let path = bundle_path(&entry(), &roots(Path::new("/Users/u")));
        assert!(path.ends_with("Applications/Example App.app"), "{path:?}");
        assert!(!path.starts_with("/Applications"));
    }

    #[test]
    fn a_written_bundle_has_the_shape_launch_services_expects() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let entry = entry();

        assert!(install(&entry, &roots).is_done());

        let bundle = bundle_path(&entry, &roots);
        let contents = bundle.join("Contents");
        assert!(contents.join("Info.plist").is_file(), "no Info.plist");

        // The executable named in the plist has to be the file that exists,
        // or macOS refuses to open the bundle.
        let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
        let name = bundle_executable_name(&entry.name);
        assert!(plist.contains(&format!("<string>{name}</string>")));
        assert!(contents.join("MacOS").join(&name).is_file(), "no bundle executable");
    }

    #[test]
    fn the_bundle_executable_is_runnable_and_starts_the_launcher() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let entry = entry();
        install(&entry, &roots);

        let trampoline = bundle_path(&entry, &roots)
            .join("Contents")
            .join("MacOS")
            .join(bundle_executable_name(&entry.name));

        let mode = std::fs::metadata(&trampoline).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "not executable: {mode:o}");

        let script = std::fs::read_to_string(&trampoline).unwrap();
        assert!(script.contains(&entry.target.to_string_lossy().to_string()), "{script}");
    }

    #[test]
    fn running_the_bundle_executable_starts_the_target() {
        // The claim this module makes is that opening the bundle opens the
        // application. Asserting on the script's text only checks that the
        // path appears in it; this runs the thing and checks what happened.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());

        let target = dir.path().join("pretend-launcher");
        std::fs::write(&target, "#!/bin/sh\necho started \"$1\"\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut entry = entry();
        entry.target = target;
        install(&entry, &roots);

        let trampoline = bundle_path(&entry, &roots)
            .join("Contents")
            .join("MacOS")
            .join(bundle_executable_name(&entry.name));

        let output = std::process::Command::new(&trampoline)
            .arg("an-argument")
            .output()
            .expect("the bundle executable did not run");

        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started an-argument");
    }

    #[test]
    fn removing_takes_the_whole_bundle_directory() {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let entry = entry();

        install(&entry, &roots);
        assert!(remove(&entry, &roots).is_done());
        assert!(!bundle_path(&entry, &roots).exists(), "the bundle survived removal");
    }
}
