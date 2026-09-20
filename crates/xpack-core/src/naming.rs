//! Naming the executables an installation keeps beside itself.
//!
//! An installation holds a launcher, an updater and an uninstaller. Named
//! after xPack, a user who opens the directory their application lives in
//! finds three programs belonging to something they have never heard of, and
//! on Windows the one in Task Manager while their application runs is called
//! `xpack-launcher.exe`. Naming them after the application is the difference
//! between a tool that installed something and a tool that disappeared.
//!
//! The names are derived once, from the display name, and then pinned: see
//! [`crate::state::InstallState::binary_base_name`] for why renaming an
//! application must not rename the files already on disk.

use crate::paths::HAS_WINDOWED_LAUNCHER;

/// Longest base name derived from a display name.
///
/// Windows resolves a path of at most 260 characters unless an application
/// opts out, and the longest name here gains `Uninstall ` and `.exe` on top
/// of whatever the installation root already costs. Sixty-four leaves room
/// for a deeply nested profile directory without truncating anything a
/// publisher is likely to have chosen.
const MAX_BASE_LEN: usize = 64;

/// What a name becomes when nothing usable survives sanitising.
const FALLBACK: &str = "Application";

/// Device names Windows resolves before it looks at the filesystem.
///
/// Opening `CON` or `NUL` reaches a device whatever directory the path names,
/// and the extension makes no difference: `NUL.exe` is still the null device.
/// A file with one of these names therefore cannot be created at all.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Makes a publisher's display name safe to use as a filename anywhere.
///
/// A display name is free-form Unicode. Spaces and accents survive, because
/// every platform xPack supports handles them and stripping them would make
/// an application's own name unrecognisable. What does not survive is
/// anything that would change where the file lands, hide it, or fail to be
/// created at all.
pub fn safe_file_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| {
            // The union of what the three platforms refuse, not what any one
            // of them does: an installation directory is written on one
            // machine and its layout is the same everywhere.
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();

    // A run of replacements reads as damage. One dash reads as punctuation.
    let mut collapsed = String::with_capacity(replaced.len());
    let mut last_was_dash = false;
    for c in replaced.chars() {
        if c == '-' {
            if !last_was_dash {
                collapsed.push(c);
            }
            last_was_dash = true;
        } else {
            collapsed.push(c);
            last_was_dash = false;
        }
    }

    // A leading dot hides the file on Unix. A trailing dot or space is
    // silently dropped by Windows, so a name stored with one would never
    // match the file that was actually created.
    let trimmed = collapsed.trim().trim_start_matches(['.', '-']).trim();
    let trimmed = trimmed.trim_end_matches(['.', ' ', '-']);

    // Truncated on a character boundary, because a display name is Unicode
    // and slicing it by bytes can split one.
    let mut capped: String = trimmed.chars().take(MAX_BASE_LEN).collect();
    while capped.ends_with(['.', ' ', '-']) {
        capped.pop();
    }

    if capped.is_empty() {
        return FALLBACK.to_string();
    }

    // A reserved name is not rejected, because the publisher's name is not
    // wrong — it just cannot be a filename on its own.
    if is_reserved(&capped) {
        return format!("{capped} App");
    }
    capped
}

/// Whether Windows would resolve this name to a device rather than a file.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    RESERVED.iter().any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

/// The names the executables of one installation carry.
///
/// Carried as a value rather than recomputed from a manifest wherever it is
/// needed, because the two must never disagree: the file on disk is found by
/// the name recorded when it was written, not by the name the current version
/// would choose today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryNames {
    /// The fixed names every installation used before this existed.
    ///
    /// Kept because an installation already on a user's machine has files
    /// with these names, and an upgrade that started looking for different
    /// ones would decide the launcher was missing and write a second.
    Xpack,
    /// Named after the application.
    Application {
        /// Already sanitised by [`safe_file_name`].
        base: String,
    },
}

impl BinaryNames {
    /// Derives names from a publisher's display name.
    pub fn from_display_name(name: &str) -> Self {
        Self::Application { base: safe_file_name(name) }
    }

    /// The base these names were derived from, if they were derived from one.
    pub fn base(&self) -> Option<&str> {
        match self {
            Self::Xpack => None,
            Self::Application { base } => Some(base),
        }
    }

    /// The console launcher.
    pub fn launcher(&self) -> String {
        self.launcher_beside_windowed(HAS_WINDOWED_LAUNCHER)
    }

    /// The console launcher, told whether a windowed build shares the
    /// directory.
    ///
    /// Split out so both answers can be tested on one machine. The Windows
    /// arm would otherwise be reasoned about rather than run, on a workspace
    /// where that has already shipped a bug.
    pub fn launcher_beside_windowed(&self, windowed_exists: bool) -> String {
        match self {
            Self::Xpack => "xpack-launcher".to_string(),
            // Where both builds exist they cannot share a name, and the
            // windowed one takes the plain application name because that is
            // the build every shortcut points at.
            Self::Application { base } if windowed_exists => format!("{base} Console"),
            Self::Application { base } => base.clone(),
        }
    }

    /// The windowed launcher, on the one platform that distinguishes it.
    pub fn windowed_launcher(&self) -> String {
        match self {
            Self::Xpack => "xpack-launcherw".to_string(),
            Self::Application { base } => base.clone(),
        }
    }

    /// The background updater.
    pub fn updater(&self) -> String {
        match self {
            Self::Xpack => "xpack-updater".to_string(),
            Self::Application { base } => format!("{base} Updater"),
        }
    }

    /// The uninstaller.
    ///
    /// Named as an instruction rather than a noun, because it is the one of
    /// these a user deliberately goes looking for and `Uninstall MyApp` is
    /// what they will be looking for.
    pub fn uninstaller(&self) -> String {
        match self {
            Self::Xpack => "xpack-uninstaller".to_string(),
            Self::Application { base } => format!("Uninstall {base}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_name_survives_intact() {
        assert_eq!(safe_file_name("My Application"), "My Application");
        assert_eq!(safe_file_name("Café Münster"), "Café Münster");
    }

    #[test]
    fn a_separator_cannot_move_the_file_somewhere_else() {
        // The whole point: a display name is publisher-controlled, and one of
        // these would otherwise create or overwrite a file in another
        // directory.
        //
        // The interior dots survive, and should: with the separators gone
        // they are ordinary characters, and removing them would mangle every
        // honest name like "Node.js Tool".
        assert_eq!(safe_file_name("evil/../../thing"), "evil-..-..-thing");
        assert_eq!(safe_file_name("C:\\Windows\\System32"), "C-Windows-System32");
        assert_eq!(safe_file_name("Node.js Tool"), "Node.js Tool");

        // What matters is that nothing left can traverse.
        for hostile in ["evil/../../thing", "C:\\Windows\\System32", "a/b/c"] {
            let safe = safe_file_name(hostile);
            assert_eq!(std::path::Path::new(&safe).components().count(), 1, "{safe}");
        }
    }

    #[test]
    fn characters_windows_refuses_are_replaced() {
        assert_eq!(safe_file_name("a*b?c\"d<e>f|g"), "a-b-c-d-e-f-g");
    }

    #[test]
    fn a_control_character_cannot_survive() {
        assert_eq!(safe_file_name("My\u{0}App\u{7}"), "My-App");
    }

    #[test]
    fn a_leading_dot_would_hide_the_file_on_unix() {
        assert_eq!(safe_file_name(".hidden"), "hidden");
        assert_eq!(safe_file_name("...hidden"), "hidden");
    }

    #[test]
    fn a_trailing_dot_or_space_is_dropped_because_windows_drops_it_silently() {
        // Stored with one and created without, every later lookup by the
        // stored name would miss the file that exists.
        assert_eq!(safe_file_name("MyApp."), "MyApp");
        assert_eq!(safe_file_name("MyApp "), "MyApp");
        assert_eq!(safe_file_name("MyApp . . "), "MyApp");
    }

    #[test]
    fn a_reserved_device_name_is_made_into_something_creatable() {
        // `NUL.exe` is the null device, not a file, so a launcher with this
        // name could not be written at all.
        assert_eq!(safe_file_name("NUL"), "NUL App");
        assert_eq!(safe_file_name("con"), "con App");
        assert_eq!(safe_file_name("COM1"), "COM1 App");
        // Only the stem is reserved, so a longer name is fine.
        assert_eq!(safe_file_name("Console"), "Console");
        assert_eq!(safe_file_name("NULL"), "NULL");
    }

    #[test]
    fn a_name_with_nothing_usable_in_it_still_produces_a_filename() {
        assert_eq!(safe_file_name(""), FALLBACK);
        assert_eq!(safe_file_name("   "), FALLBACK);
        assert_eq!(safe_file_name("///"), FALLBACK);
        assert_eq!(safe_file_name("..."), FALLBACK);
    }

    #[test]
    fn a_long_name_is_capped_without_splitting_a_character() {
        let long = "é".repeat(200);
        let capped = safe_file_name(&long);
        assert_eq!(capped.chars().count(), MAX_BASE_LEN);
        // Sliced by bytes this would panic or produce invalid UTF-8.
        assert!(capped.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_four_names_are_distinct_on_every_platform() {
        let names = BinaryNames::from_display_name("My App");
        for windowed in [true, false] {
            let mut all = vec![
                names.launcher_beside_windowed(windowed),
                names.updater(),
                names.uninstaller(),
            ];
            if windowed {
                all.push(names.windowed_launcher());
            }
            let count = all.len();
            all.sort();
            all.dedup();
            assert_eq!(all.len(), count, "two binaries would share a name: {all:?}");
        }
    }

    #[test]
    fn the_windowed_build_takes_the_plain_name_because_shortcuts_point_at_it() {
        let names = BinaryNames::from_display_name("My App");
        assert_eq!(names.windowed_launcher(), "My App");
        assert_eq!(names.launcher_beside_windowed(true), "My App Console");
    }

    #[test]
    fn the_only_launcher_takes_the_plain_name_where_there_is_no_windowed_build() {
        let names = BinaryNames::from_display_name("My App");
        assert_eq!(names.launcher_beside_windowed(false), "My App");
    }

    #[test]
    fn the_uninstaller_reads_as_the_thing_a_user_goes_looking_for() {
        let names = BinaryNames::from_display_name("My App");
        assert_eq!(names.uninstaller(), "Uninstall My App");
        assert_eq!(names.updater(), "My App Updater");
    }

    #[test]
    fn the_legacy_names_are_exactly_what_earlier_installations_have_on_disk() {
        // An installation already on a user's machine has these files. An
        // upgrade looking for anything else would decide the launcher was
        // missing and write a second one beside it.
        let names = BinaryNames::Xpack;
        assert_eq!(names.launcher_beside_windowed(false), "xpack-launcher");
        assert_eq!(names.launcher_beside_windowed(true), "xpack-launcher");
        assert_eq!(names.windowed_launcher(), "xpack-launcherw");
        assert_eq!(names.updater(), "xpack-updater");
        assert_eq!(names.uninstaller(), "xpack-uninstaller");
        assert_eq!(names.base(), None);
    }

    #[test]
    fn a_derived_name_is_already_sanitised_when_it_is_read_back() {
        let names = BinaryNames::from_display_name("Tom & Jerry/../x");
        assert_eq!(names.base(), Some("Tom & Jerry-..-x"));
        assert!(!names.uninstaller().contains('/'));
        assert!(!names.uninstaller().contains('\\'));
    }

    #[test]
    fn deriving_twice_from_one_name_gives_one_answer() {
        // Pinning in state only helps if the derivation is a function of the
        // name and nothing else.
        for name in ["My App", "NUL", "", "a/b", "é".repeat(100).as_str()] {
            assert_eq!(safe_file_name(name), safe_file_name(name));
        }
    }
}
