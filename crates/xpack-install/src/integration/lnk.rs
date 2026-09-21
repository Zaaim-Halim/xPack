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

/// `HasLinkTargetIDList`: the shortcut carries a shell item list.
const HAS_LINK_TARGET_ID_LIST: u32 = 0x0000_0001;
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

/// `CLSID_MyComputer`, the root every filesystem item list hangs from.
///
/// Byte order is the on-the-wire layout of a GUID: the first three groups
/// little-endian, the last two as written.
const CLSID_MY_COMPUTER: [u8; 16] = [
    0xE0, 0x4F, 0xD0, 0x20, 0xEA, 0x3A, 0x69, 0x10, 0xA2, 0xD8, 0x08, 0x00, 0x2B, 0x30, 0x30, 0x9D,
];

/// Shell item type for the root node carrying a CLSID.
const ITEM_ROOT: u8 = 0x1F;
/// Shell item type for a drive, such as `C:\`.
const ITEM_DRIVE: u8 = 0x2F;
/// Shell item type for a directory.
const ITEM_DIRECTORY: u8 = 0x31;
/// Shell item type for a file.
const ITEM_FILE: u8 = 0x32;

/// `FILE_ATTRIBUTE_DIRECTORY`.
const ATTRIBUTE_DIRECTORY: u16 = 0x0010;
/// `FILE_ATTRIBUTE_ARCHIVE`, which is what an ordinary file carries.
const ATTRIBUTE_ARCHIVE: u16 = 0x0020;

/// Signature of the extension block carrying an item's long name.
const EXTENSION_SIGNATURE: u32 = 0xBEEF_0004;
/// Extension block version. `0x0003` is the oldest layout every shell reads,
/// and the one with no fields this writer would have to invent.
const EXTENSION_VERSION: u16 = 0x0003;

/// Builds the `LinkTargetIDList`: the shell's own name for the target.
///
/// # Why this exists as well as `LinkInfo`
///
/// `LinkInfo` carries the target's absolute path as text, and the shell will
/// resolve a shortcut from it. What it will not do is *report* it: the path a
/// caller gets back from `IShellLink::GetPath` — which is what Explorer's
/// properties dialog, "open file location", pinning, and every script using
/// `WScript.Shell` read — comes from this item list. Without one, those all
/// see a shortcut with no target, even where double-clicking still works.
///
/// So both are written. They say the same thing in the two vocabularies the
/// shell has for saying it.
///
/// # What an item list is
///
/// A chain from the desktop down to the file: My Computer, the drive, each
/// directory, then the file itself. Each link is an `ItemID` — a length, a
/// type byte, and the type's own payload — and a zero length ends the chain.
///
/// Each directory and file item carries its name twice: once in the system
/// code page, which is what shells older than Windows XP read, and once as
/// UTF-16 in an extension block, which is what every shell since reads and
/// the only one of the two that can hold a name outside the code page.
fn target_id_list(target: &str) -> Vec<u8> {
    let mut items = Vec::new();

    // My Computer. A file's item list is meaningless without the root it
    // descends from: the shell resolves each item against its parent.
    let mut root = vec![ITEM_ROOT, 0x50];
    root.extend_from_slice(&CLSID_MY_COMPUTER);
    push_item(&mut items, &root);

    let mut components = target.split('\\').filter(|part| !part.is_empty());
    let Some(drive) = components.next() else {
        return Vec::new();
    };

    // `C:\`, padded to the fixed width the drive item has always had.
    let mut drive_item = vec![ITEM_DRIVE];
    drive_item.extend_from_slice(format!("{drive}\\").as_bytes());
    drive_item.resize(23, 0);
    push_item(&mut items, &drive_item);

    let rest: Vec<&str> = components.collect();
    let last = rest.len().saturating_sub(1);
    for (index, component) in rest.iter().enumerate() {
        let is_file = index == last;
        push_item(&mut items, &filesystem_item(component, is_file));
    }

    // The terminating zero-length ItemID.
    items.extend_from_slice(&0u16.to_le_bytes());

    let mut out = Vec::with_capacity(items.len() + 2);
    let size = u16::try_from(items.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&items);
    out
}

/// Appends one `ItemID`: its size, including the two bytes of the size itself.
fn push_item(items: &mut Vec<u8>, data: &[u8]) {
    let size = u16::try_from(data.len() + 2).unwrap_or(u16::MAX);
    items.extend_from_slice(&size.to_le_bytes());
    items.extend_from_slice(data);
}

/// Builds a directory or file item, carrying its name in both encodings.
fn filesystem_item(name: &str, is_file: bool) -> Vec<u8> {
    let mut item = vec![if is_file { ITEM_FILE } else { ITEM_DIRECTORY }, 0x00];
    // Size and timestamps of the target, which the shell treats as advisory
    // and re-reads from the filesystem. Zero rather than a guess.
    item.extend_from_slice(&0u32.to_le_bytes());
    item.extend_from_slice(&0u16.to_le_bytes());
    item.extend_from_slice(&0u16.to_le_bytes());
    item.extend_from_slice(
        &(if is_file { ATTRIBUTE_ARCHIVE } else { ATTRIBUTE_DIRECTORY }).to_le_bytes(),
    );

    let primary = to_code_page(name);
    item.extend_from_slice(&primary);
    item.push(0);
    // The extension block that follows must start on an even offset, counted
    // from the beginning of the ItemID — which the two size bytes precede.
    if (item.len() + 2) % 2 != 0 {
        item.push(0);
    }

    let extension_offset = u16::try_from(item.len() + 2).unwrap_or(u16::MAX);
    item.extend_from_slice(&long_name_extension(name, extension_offset));
    // A zero size where the next extension block's size would be: the list of
    // extension blocks ends here.
    item.extend_from_slice(&0u16.to_le_bytes());
    item
}

/// The `0xBEEF0004` extension block, which carries the item's real name.
///
/// `offset` is where this block begins within its `ItemID`, which the block
/// repeats at its end so a reader walking backwards can find it.
fn long_name_extension(name: &str, offset: u16) -> Vec<u8> {
    let mut block = Vec::new();
    // Size is filled in once the rest is known.
    block.extend_from_slice(&0u16.to_le_bytes());
    block.extend_from_slice(&EXTENSION_VERSION.to_le_bytes());
    block.extend_from_slice(&EXTENSION_SIGNATURE.to_le_bytes());
    // Creation and last-access dates and times, in MS-DOS form. Zero for the
    // same reason as the header's timestamps.
    block.extend_from_slice(&0u16.to_le_bytes());
    block.extend_from_slice(&0u16.to_le_bytes());
    block.extend_from_slice(&0u16.to_le_bytes());
    block.extend_from_slice(&0u16.to_le_bytes());
    // Unknown, and documented as such. Zero is what the shell writes when it
    // has nothing to put here.
    block.extend_from_slice(&0u16.to_le_bytes());
    block.extend_from_slice(&to_utf16_nul(name));
    block.extend_from_slice(&offset.to_le_bytes());

    let size = u16::try_from(block.len()).unwrap_or(u16::MAX);
    block[0..2].copy_from_slice(&size.to_le_bytes());
    block
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
    let mut flags = HAS_LINK_TARGET_ID_LIST | HAS_LINK_INFO | HAS_WORKING_DIR | IS_UNICODE;
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
    // The item list comes first: the specification fixes this order, and it is
    // what the shell reads before anything else.
    out.extend_from_slice(&target_id_list(&shortcut.target));
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

    /// Where `LinkInfo` begins, which is after the header and the item list.
    ///
    /// Computed rather than written down: the item list is variable length,
    /// and a test asserting a fixed offset would be asserting the length of
    /// one particular path.
    fn link_info_start(bytes: &[u8]) -> usize {
        let id_list = u16::from_le_bytes(bytes[76..78].try_into().unwrap()) as usize;
        78 + id_list
    }

    /// Walks the `LinkTargetIDList` that follows the header, returning each
    /// `ItemID`'s payload.
    fn id_list_items(bytes: &[u8]) -> Vec<Vec<u8>> {
        let size = u16::from_le_bytes(bytes[76..78].try_into().unwrap()) as usize;
        let list = &bytes[78..78 + size];
        let mut items = Vec::new();
        let mut at = 0;
        loop {
            let item = u16::from_le_bytes(list[at..at + 2].try_into().unwrap()) as usize;
            if item == 0 {
                break;
            }
            items.push(list[at + 2..at + item].to_vec());
            at += item;
        }
        items
    }

    /// The UTF-16 name inside an item's `0xBEEF0004` extension block.
    fn long_name_of(item: &[u8]) -> String {
        let signature = item
            .windows(4)
            .position(|window| window == EXTENSION_SIGNATURE.to_le_bytes())
            .expect("an extension block");
        // signature, then two dates, two times and the unknown field.
        let mut at = signature + 4 + 10;
        let mut units = Vec::new();
        while at + 1 < item.len() {
            let unit = u16::from_le_bytes(item[at..at + 2].try_into().unwrap());
            if unit == 0 {
                break;
            }
            units.push(unit);
            at += 2;
        }
        String::from_utf16(&units).expect("valid UTF-16")
    }

    #[test]
    fn the_shortcut_carries_a_target_id_list() {
        // Without one the shell resolves the link but reports no target at
        // all: Explorer's properties dialog, "open file location", pinning and
        // every script reading TargetPath see a shortcut pointing nowhere.
        let bytes = serialise(&shortcut());
        assert_eq!(read_u32(&bytes, 20) & HAS_LINK_TARGET_ID_LIST, HAS_LINK_TARGET_ID_LIST);
    }

    #[test]
    fn the_id_list_descends_from_my_computer_through_every_directory() {
        let bytes = serialise(&shortcut());
        let items = id_list_items(&bytes);

        // My Computer, the drive, six directories, then the file.
        assert_eq!(items.len(), 9, "{items:?}");
        assert_eq!(items[0][0], ITEM_ROOT);
        assert_eq!(items[0][2..18], CLSID_MY_COMPUTER);
        assert_eq!(items[1][0], ITEM_DRIVE);
        assert_eq!(&items[1][1..4], b"C:\\");

        let names: Vec<String> = items[2..].iter().map(|item| long_name_of(item)).collect();
        assert_eq!(
            names,
            ["Users", "u", "AppData", "Local", "xpack", "com.example.app", "xpack-launcherw.exe"],
            "{names:?}"
        );
    }

    #[test]
    fn only_the_last_item_is_a_file() {
        let bytes = serialise(&shortcut());
        let items = id_list_items(&bytes);
        let (file, directories) = items[2..].split_last().expect("at least one item");

        assert_eq!(file[0], ITEM_FILE);
        for directory in directories {
            assert_eq!(directory[0], ITEM_DIRECTORY, "{directory:?}");
        }
    }

    #[test]
    fn every_item_id_declares_its_own_length() {
        // A reader walks this list by length alone. One wrong size and every
        // item after it is read from the middle of the one before.
        let bytes = serialise(&shortcut());
        let size = u16::from_le_bytes(bytes[76..78].try_into().unwrap()) as usize;
        let list = &bytes[78..78 + size];

        let mut at = 0;
        let mut items = 0;
        loop {
            let item = u16::from_le_bytes(list[at..at + 2].try_into().unwrap()) as usize;
            if item == 0 {
                break;
            }
            assert!(item >= 2 && at + item <= list.len(), "item {items} claims {item} bytes");
            at += item;
            items += 1;
        }
        assert_eq!(at + 2, list.len(), "the list does not end where its size says it does");
    }

    #[test]
    fn a_name_outside_the_code_page_survives_in_the_extension_block() {
        // The same reason the Unicode LocalBasePath exists: the code-page
        // field cannot hold this name, and the shortcut still has to work.
        let bytes = serialise(&Shortcut::new(Path::new(r"C:\Users\José Ramírez\app.exe")));
        let items = id_list_items(&bytes);
        let names: Vec<String> = items[2..].iter().map(|item| long_name_of(item)).collect();
        assert_eq!(names, ["Users", "José Ramírez", "app.exe"], "{names:?}");
    }

    #[test]
    fn an_extension_block_starts_on_an_even_offset() {
        // The shell reads the block at the offset the item records, and reads
        // it as aligned words.
        for target in [r"C:\a\b.exe", r"C:\ab\cd.exe", r"C:\abc\def.exe"] {
            let bytes = serialise(&Shortcut::new(Path::new(target)));
            for item in id_list_items(&bytes).into_iter().skip(2) {
                let signature = item
                    .windows(4)
                    .position(|window| window == EXTENSION_SIGNATURE.to_le_bytes())
                    .expect("an extension block");
                // The block begins two bytes before its signature, and the
                // item's own two size bytes precede everything.
                assert_eq!((signature - 2 + 2) % 2, 0, "{target}: {item:?}");
            }
        }
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
        let link_info_start = link_info_start(&bytes);
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

        let start = link_info_start(&bytes);
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
        let start = link_info_start(&bytes);

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
