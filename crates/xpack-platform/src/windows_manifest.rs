//! The application manifest every xPack program with a window carries.
//!
//! Built into the installer and the update notice when they are linked, and
//! written again by `xpack installer` when it brands an executable, from this
//! one file, so the two can never disagree.
//!
//! Built in, not only branded in: without Common Controls 6 declared, Windows
//! loads the version of that library from before Windows XP, which lacks
//! functions the windowing code imports, and refuses to start the program at
//! all. An executable used as it was built, never branded, has to start too.

/// The manifest itself.
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
/// * **Common Controls 6.** Needed to start at all, as the module
///   documentation explains, and the current look of buttons and checkboxes
///   rather than the one from Windows 95.
pub const WINDOWS_MANIFEST: &str = include_str!("../windows.manifest");

#[cfg(test)]
mod tests {
    use super::WINDOWS_MANIFEST as MANIFEST;

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
}
