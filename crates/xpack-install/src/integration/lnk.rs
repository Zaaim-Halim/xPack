//! Writing a Windows shortcut file.
//!
//! A `.lnk` is a documented binary format — [MS-SHLLINK] — and this module
//! serialises one directly. The conventional route is COM: `CoCreateInstance`
//! an `IShellLink`, set its properties, persist it through `IPersistFile`.
//! That is unsafe FFI and a large dependency, and this workspace forbids
//! unsafe code. Emitting the bytes needs neither, and the format is stable:
//! the structures below have not changed since Windows 2000.
//!
//! [MS-SHLLINK]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-shllink/
//!
//! # What is written, and what is deliberately not
//!
//! A shortcut can carry a `LinkTargetIDList` — a shell item list naming the
//! desktop, the drive, each folder and finally the file. It is what Explorer
//! prefers, and building one means constructing PIDLs, a format that is only
//! semi-documented and genuinely fiddly to get right.
//!
//! This writer emits a `LinkInfo` block instead, which carries the absolute
//! path of the target and is the documented fallback the shell resolves from.
//! That is the trade: a simpler, verifiable structure, at the cost of not
//! being the shell's first-choice representation.
//!
//! Both the ANSI and the Unicode path fields are written. The ANSI field
//! cannot represent a path outside the system code page, and a user whose
//! profile directory contains non-ASCII characters is not unusual, so the
//! Unicode field is what makes those installations work.
//!
//! # This is the one thing here that cannot be proven on a Mac
//!
//! Every structure below is unit-tested field by field against the
//! specification, and those tests run everywhere. What they cannot prove is
//! that Windows Explorer *resolves* the result. That is checked in CI on a
//! real Windows runner, which reads the file back through `WScript.Shell` and
//! compares the target it recovers.

use std::path::Path;

/// `HasLinkInfo`: the shortcut carries a `LinkInfo` structure.
const HAS_LINK_INFO: u32 = 0x0000_0002;
/// `HasName`: the shortcut carries a description string.
const HAS_NAME: u32 = 0x0000_0004;
/// `HasWorkingDir`: the shortcut carries a working directory.
const HAS_WORKING_DIR: u32 = 0x0000_0010;
/// `HasArguments`: the shortcut carries command-line arguments.
const HAS_ARGUMENTS: u32 = 0x0000_0020;
/// `HasIconLocation`: the shortcut names an icon file.
const HAS_ICON_LOCATION: u32 = 0x0000_0040;
/// `IsUnicode`: every string is UTF-16 rather than system-code-page bytes.
const IS_UNICODE: u32 = 0x0000_0080;

/// `FILE_ATTRIBUTE_NORMAL`.
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
/// `SW_SHOWNORMAL`: open the application in an ordinary window.
const SW_SHOWNORMAL: u32 = 1;
/// `DRIVE_FIXED`. The install root is on a local disk by construction.
const DRIVE_FIXED: u32 = 3;

/// `VolumeIDAndLocalBasePath`: the `LinkInfo` carries a local path.
const VOLUME_ID_AND_LOCAL_BASE_PATH: u32 = 0x0000_0001;

/// The shell link class identifier, `00021401-0000-0000-C000-000000000046`.
///
/// Stored in the mixed-endian form a `GUID` uses on disk: the first three
/// fields little-endian, the last eight bytes in order.
const LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];

/// Size of `ShellLinkHeader`, which the format requires to be exactly this.
const HEADER_SIZE: u32 = 0x4C;

/// Size of a `LinkInfo` header that includes the Unicode path offsets.
///
/// Nine 32-bit fields. The shell reads the Unicode fields only when the header
/// is at least this large, which is why the shorter 0x1C form is not used.
const LINK_INFO_HEADER_SIZE: u32 = 0x24;

/// A shortcut to be written.
#[derive(Debug, Clone)]
pub struct Shortcut {
    /// Absolute path of the program the shortcut starts.
    pub target: String,
    /// Directory the program starts in.
    pub working_directory: String,
    /// Arguments passed to the program, already quoted as one string.
    pub arguments: Option<String>,
    /// Absolute path of the file holding the icon.
    pub icon: Option<String>,
    /// Description, shown by Explorer as the shortcut's comment.
    pub description: Option<String>,
}

impl Shortcut {
    /// Builds a shortcut that starts `target` in its own directory.
    pub fn new(target: &Path) -> Self {
        let target = target.to_string_lossy().into_owned();
        let working_directory = parent_of(&target);
        Self { target, working_directory, arguments: None, icon: None, description: None }
    }

    /// Sets the icon file.
    #[must_use]
    pub fn with_icon(mut self, icon: Option<&Path>) -> Self {
        self.icon = icon.map(|p| p.to_string_lossy().into_owned());
        self
    }

    /// Sets the description Explorer shows as a comment.
    #[must_use]
    pub fn with_description(mut self, description: Option<&str>) -> Self {
        self.description = description.map(str::to_string);
        self
    }

    /// Serialises this shortcut to the bytes of a `.lnk` file.
    pub fn to_bytes(&self) -> Vec<u8> {
        serialise(self)
    }
}

/// The directory part of a Windows path.
///
/// Deliberately not `Path::parent`. That answers using the *host's* path
/// rules, and on Unix a backslash is an ordinary character — so
/// `C:\apps\thing\run.exe` has no parent at all there and the shortcut
/// would be written with an empty working directory. This file describes a
/// Windows artefact and must read Windows paths the same way wherever it is
/// built, which is also what makes the behaviour testable off Windows.
fn parent_of(path: &str) -> String {
    let bytes = path.as_bytes();
    let Some(index) = path.rfind(['\\', '/']) else {
        return String::new();
    };

    // Two cases where the separator *is* the directory and must be kept.
    // `C:` alone means "the current directory on drive C", which is not the
    // same place as `C:\` and is not even a fixed place; `` alone would be
    // relative rather than rooted.
    let is_drive_root = index == 2 && bytes.len() > 2 && bytes[1] == b':';
    if index == 0 || is_drive_root {
        return path[..=index].to_string();
    }
    path[..index].to_string()
}

/// Writes the 76-byte `ShellLinkHeader`.
fn write_header(out: &mut Vec<u8>, flags: u32) {
    out.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&LINK_CLSID);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&FILE_ATTRIBUTE_NORMAL.to_le_bytes());
    // Creation, access and write times. Zero means "not recorded", which is
    // valid and avoids a shortcut whose contents change every time it is
    // rewritten for reasons that have nothing to do with the shortcut.
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    // FileSize of the target. Advisory only; the shell does not rely on it.
    out.extend_from_slice(&0u32.to_le_bytes());
    // IconIndex, into the icon file named by ICON_LOCATION.
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&SW_SHOWNORMAL.to_le_bytes());
    // HotKey, then the three reserved fields, all of which must be zero.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
}

/// Builds the `LinkInfo` structure naming the target's absolute path.
fn link_info(target: &str) -> Vec<u8> {
    let ansi_path = to_code_page(target);
    let unicode_path = to_utf16_nul(target);

    // VolumeID: size, drive type, serial, label offset, then an empty label.
    let volume_id_size: u32 = 0x11;
    let mut volume = Vec::new();
    volume.extend_from_slice(&volume_id_size.to_le_bytes());
    volume.extend_from_slice(&DRIVE_FIXED.to_le_bytes());
    // Serial number. Zero rather than invented: the shell tolerates it, and a
    // fabricated one would be a claim about the user's disk that is not true.
    volume.extend_from_slice(&0u32.to_le_bytes());
    volume.extend_from_slice(&0x10u32.to_le_bytes());
    volume.push(0); // empty VolumeLabel

    let header_len = LINK_INFO_HEADER_SIZE as usize;
    let volume_offset = header_len;
    let local_base_path_offset = volume_offset + volume.len();
    let common_path_suffix_offset = local_base_path_offset + ansi_path.len();
    // CommonPathSuffix is a single terminating null: the whole path is in
    // LocalBasePath, so there is no suffix to append to it.
    let local_base_path_offset_unicode = common_path_suffix_offset + 1;
    let common_path_suffix_offset_unicode = local_base_path_offset_unicode + unicode_path.len();
    let total = common_path_suffix_offset_unicode + 2;

    let mut out = Vec::with_capacity(total);
    let u32_of = |value: usize| u32::try_from(value).unwrap_or(u32::MAX).to_le_bytes();

    out.extend_from_slice(&u32_of(total));
    out.extend_from_slice(&LINK_INFO_HEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&VOLUME_ID_AND_LOCAL_BASE_PATH.to_le_bytes());
    out.extend_from_slice(&u32_of(volume_offset));
    out.extend_from_slice(&u32_of(local_base_path_offset));
    // CommonNetworkRelativeLinkOffset. Zero: this is a local path.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&u32_of(common_path_suffix_offset));
    out.extend_from_slice(&u32_of(local_base_path_offset_unicode));
    out.extend_from_slice(&u32_of(common_path_suffix_offset_unicode));

    out.extend_from_slice(&volume);
    out.extend_from_slice(&ansi_path);
    out.push(0); // CommonPathSuffix
    out.extend_from_slice(&unicode_path);
    out.extend_from_slice(&0u16.to_le_bytes()); // CommonPathSuffixUnicode

    debug_assert_eq!(out.len(), total, "LinkInfo offsets disagree with what was written");
    out
}

/// Writes one `StringData` entry: a UTF-16 length followed by the characters.
///
/// Not null-terminated — the count is the only terminator the format has, and
/// a writer that adds one produces a string with a trailing null inside it.
fn write_string_data(out: &mut Vec<u8>, value: &str) {
    let units: Vec<u16> = value.encode_utf16().collect();
    let count = u16::try_from(units.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for unit in units.iter().take(count as usize) {
        out.extend_from_slice(&unit.to_le_bytes());
    }
}

/// Encodes a path for the ANSI `LocalBasePath` field, as best it can.
///
/// The field is defined as system-code-page bytes, and there is no portable
/// way to produce those. Anything outside ASCII becomes `?`, which is what
/// Windows itself substitutes when it cannot represent a character in the
/// current code page.
///
/// This being lossy is exactly why the Unicode field beside it is written: a
/// shell that reads the Unicode field never sees this one.
fn to_code_page(value: &str) -> Vec<u8> {
    value.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect()
}

/// Encodes a string as null-terminated UTF-16 little-endian bytes.
fn to_utf16_nul(value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() * 2 + 2);
    for unit in value.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Serialises a shortcut, with the string blocks in specification order.
///
/// Separate from [`Shortcut::to_bytes`] so the ordering rule has one home.
/// The blocks are positional: nothing in the data says which string is which,
/// so a writer that emits them out of order produces a file in which the
/// working directory is read as the arguments.
pub fn serialise(shortcut: &Shortcut) -> Vec<u8> {
    let mut flags = HAS_LINK_INFO | HAS_WORKING_DIR | IS_UNICODE;
    if shortcut.description.is_some() {
        flags |= HAS_NAME;
    }
    if shortcut.arguments.is_some() {
        flags |= HAS_ARGUMENTS;
    }
    if shortcut.icon.is_some() {
        flags |= HAS_ICON_LOCATION;
    }

    let mut out = Vec::new();
    write_header(&mut out, flags);
    out.extend_from_slice(&link_info(&shortcut.target));

    // NAME_STRING, RELATIVE_PATH, WORKING_DIR, COMMAND_LINE_ARGUMENTS,
    // ICON_LOCATION — in that order, and only those whose flag is set.
    // RELATIVE_PATH is never written: it is an alternative to the absolute
    // path already in LinkInfo, and supplying both invites them to disagree.
    if let Some(description) = &shortcut.description {
        write_string_data(&mut out, description);
    }
    write_string_data(&mut out, &shortcut.working_directory);
    if let Some(arguments) = &shortcut.arguments {
        write_string_data(&mut out, arguments);
    }
    if let Some(icon) = &shortcut.icon {
        write_string_data(&mut out, icon);
    }

    // The terminal block. A reader walks ExtraData until it meets a size
    // below 4, so this zero is what says the file ends here.
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn shortcut() -> Shortcut {
        Shortcut::new(Path::new(
            r"C:\Users\u\AppData\Local\xpack\com.example.app\xpack-launcherw.exe",
        ))
    }

    #[test]
    fn the_header_is_the_size_and_class_the_format_requires() {
        // A reader rejects the file outright if either is wrong, so these are
        // the two bytes-exact constants worth pinning.
        let bytes = serialise(&shortcut());
        assert_eq!(read_u32(&bytes, 0), 0x4C);
        assert_eq!(&bytes[4..20], &LINK_CLSID);
    }

    #[test]
    fn the_unicode_flag_is_set_because_every_string_is_utf16() {
        let bytes = serialise(&shortcut());
        let flags = read_u32(&bytes, 20);
        assert_eq!(flags & IS_UNICODE, IS_UNICODE, "flags {flags:#x}");
        assert_eq!(flags & HAS_LINK_INFO, HAS_LINK_INFO);
        assert_eq!(flags & HAS_WORKING_DIR, HAS_WORKING_DIR);
    }

    #[test]
    fn flags_describe_exactly_the_optional_blocks_that_were_written() {
        // A flag set without its block, or a block without its flag, shifts
        // everything after it and the shell reads the wrong string.
        let bare = serialise(&shortcut());
        let bare_flags = read_u32(&bare, 20);
        assert_eq!(bare_flags & HAS_ICON_LOCATION, 0);
        assert_eq!(bare_flags & HAS_NAME, 0);
        assert_eq!(bare_flags & HAS_ARGUMENTS, 0);

        let decorated = serialise(
            &shortcut()
                .with_icon(Some(Path::new(r"C:\x\icon.ico")))
                .with_description(Some("Example")),
        );
        let flags = read_u32(&decorated, 20);
        assert_eq!(flags & HAS_ICON_LOCATION, HAS_ICON_LOCATION);
        assert_eq!(flags & HAS_NAME, HAS_NAME);
    }

    #[test]
    fn the_link_info_declares_its_own_length_correctly() {
        // Every offset inside LinkInfo is relative to its start, so a wrong
        // total makes the shell read the strings from the wrong place.
        let bytes = serialise(&shortcut());
        let link_info_start = 0x4C;
        let declared = read_u32(&bytes, link_info_start) as usize;
        let header_size = read_u32(&bytes, link_info_start + 4);

        assert_eq!(header_size, LINK_INFO_HEADER_SIZE);
        assert!(link_info_start + declared < bytes.len(), "LinkInfo runs past the file");
    }

    #[test]
    fn the_target_path_is_recoverable_from_the_unicode_field() {
        // This is the field Windows actually reads on any modern system, and
        // the one that carries a path the ANSI field cannot represent.
        let target = r"C:\Users\José\app\xpack-launcherw.exe";
        let bytes = serialise(&Shortcut::new(Path::new(target)));

        let start = 0x4C;
        let offset = read_u32(&bytes, start + 28) as usize;
        let from = start + offset;
        let units: Vec<u16> = bytes[from..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|&u| u != 0)
            .collect();

        assert_eq!(String::from_utf16(&units).unwrap(), target);
    }

    #[test]
    fn a_non_ascii_path_degrades_in_the_ansi_field_and_survives_in_the_unicode_one() {
        let target = r"C:\Users\José\app.exe";
        let bytes = serialise(&Shortcut::new(Path::new(target)));
        let start = 0x4C;

        let ansi_at = start + read_u32(&bytes, start + 16) as usize;
        let ansi: Vec<u8> = bytes[ansi_at..].iter().copied().take_while(|&b| b != 0).collect();
        let ansi = String::from_utf8(ansi).unwrap();

        assert!(ansi.contains("Jos?"), "expected a substitution, got {ansi:?}");
        assert_eq!(
            ansi.len(),
            target.chars().count(),
            "the ANSI field must be one byte per character, or every offset after it is wrong"
        );
    }

    #[test]
    fn string_data_is_counted_rather_than_null_terminated() {
        // A writer that null-terminates produces a string containing a
        // trailing null, which Explorer shows verbatim.
        let mut out = Vec::new();
        write_string_data(&mut out, "ab");
        assert_eq!(out, vec![2, 0, b'a', 0, b'b', 0]);
    }

    #[test]
    fn the_file_ends_with_the_terminal_block() {
        let bytes = serialise(&shortcut());
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn the_working_directory_defaults_to_the_targets_own_directory() {
        // Read with Windows rules whatever host this is built on. Using
        // `Path::parent` here gave an empty working directory on Unix,
        // because a backslash is an ordinary character there.
        let s = Shortcut::new(Path::new(r"C:\apps\thing\run.exe"));
        assert_eq!(s.working_directory, r"C:\apps\thing");
    }

    #[test]
    fn a_target_at_the_root_keeps_its_separator() {
        // Trimming it would turn `C:\` into `C:`, which Windows resolves as
        // "the current directory on drive C" rather than the drive itself.
        // `C:` without the separator means "the current directory on drive
        // C", which is not a fixed location and is not the drive root.
        assert_eq!(parent_of(r"C:\run.exe"), r"C:\");
        assert_eq!(parent_of(r"\run.exe"), r"\");
        assert_eq!(parent_of("run.exe"), "");
        assert_eq!(parent_of(r"C:\apps\run.exe"), r"C:\apps");
    }

    #[test]
    fn an_empty_optional_string_is_absent_rather_than_empty() {
        // An ICON_LOCATION of zero characters makes Explorer show a blank
        // icon instead of falling back to the executable's own.
        let bytes = serialise(&shortcut().with_icon(None));
        assert_eq!(read_u32(&bytes, 20) & HAS_ICON_LOCATION, 0);
    }
}
