//! Which platform a program file was built for, read from its header.
//!
//! An installer carries an installer program and the runtime programs it
//! places. Built on one machine for another, nothing about the file names
//! says which platform a program belongs to, and a program for the wrong one
//! is not noticed until a user's machine refuses to run it. Every executable
//! format records its operating system and processor in a fixed place, so
//! the mismatch is caught here instead, while building.

use std::io::Read;
use std::path::Path;

use xpack_core::{Arch, Error, Os, Platform, Result};

/// What a program's header says it was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuiltFor {
    pub os: Os,
    /// The processors it runs on: one for an ordinary program, several for a
    /// macOS universal one, none when the header names a processor xPack does
    /// not build for.
    pub archs: Vec<Arch>,
}

/// How much of a file is read: enough for every header looked at here,
/// including a PE header placed after a long DOS stub.
const HEADER_BYTES: usize = 64 * 1024;

/// What `bytes`, the start of a file, say it was built for; `None` when they
/// are not a Windows, Linux or macOS program at all.
pub(crate) fn built_for(bytes: &[u8]) -> Option<BuiltFor> {
    if bytes.starts_with(b"MZ") {
        return pe(bytes);
    }
    if bytes.starts_with(b"\x7fELF") {
        return elf(bytes);
    }
    mach_o(bytes)
}

/// Refuses a program that is not built for `target`, naming what it is.
///
/// `role` says what the file is for, so the message reads "the launcher …"
/// rather than naming a path alone.
pub(crate) fn ensure_built_for(path: &Path, target: Platform, role: &str) -> Result<()> {
    let mut head = Vec::with_capacity(HEADER_BYTES);
    std::fs::File::open(path)
        .and_then(|file| file.take(HEADER_BYTES as u64).read_to_end(&mut head))
        .map_err(|e| Error::io(path, e))?;

    let Some(found) = built_for(&head) else {
        return Err(Error::invalid(
            role,
            format!("{} is not a {} program", path.display(), target.os),
        ));
    };
    if found.os != target.os {
        return Err(Error::invalid(
            role,
            format!(
                "{} is a {} program, but this installer is for {target}",
                path.display(),
                found.os
            ),
        ));
    }
    if !found.archs.contains(&target.arch) {
        let archs = if found.archs.is_empty() {
            "a processor xPack does not build for".to_string()
        } else {
            found.archs.iter().map(ToString::to_string).collect::<Vec<_>>().join(" and ")
        };
        return Err(Error::invalid(
            role,
            format!("{} is built for {archs}, but this installer is for {target}", path.display()),
        ));
    }
    Ok(())
}

fn u16_le(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u32_be(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// A Windows program: the DOS header points at the PE header, whose first
/// field after the signature is the machine.
fn pe(bytes: &[u8]) -> Option<BuiltFor> {
    let pe = usize::try_from(u32_le(bytes, 0x3C)?).ok()?;
    if bytes.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let arch = match u16_le(bytes, pe + 4)? {
        0x8664 => Some(Arch::X64),
        0xAA64 => Some(Arch::Arm64),
        _ => None,
    };
    Some(BuiltFor { os: Os::Windows, archs: arch.into_iter().collect() })
}

/// A Linux program: `e_machine`, at a fixed offset, in the byte order the
/// header declares.
fn elf(bytes: &[u8]) -> Option<BuiltFor> {
    let raw = bytes.get(0x12..0x14)?;
    let machine = match bytes.get(5)? {
        1 => u16::from_le_bytes([raw[0], raw[1]]),
        2 => u16::from_be_bytes([raw[0], raw[1]]),
        _ => return None,
    };
    let arch = match machine {
        0x3E => Some(Arch::X64),
        0xB7 => Some(Arch::Arm64),
        _ => None,
    };
    Some(BuiltFor { os: Os::Linux, archs: arch.into_iter().collect() })
}

/// The Mach-O CPU types for the processors xPack builds for.
const CPU_X86_64: u32 = 0x0100_0007;
const CPU_ARM64: u32 = 0x0100_000C;

fn mach_arch(cpu: u32) -> Option<Arch> {
    match cpu {
        CPU_X86_64 => Some(Arch::X64),
        CPU_ARM64 => Some(Arch::Arm64),
        _ => None,
    }
}

/// A macOS program: a single-architecture Mach-O, or a universal one listing
/// several.
fn mach_o(bytes: &[u8]) -> Option<BuiltFor> {
    match u32_le(bytes, 0)? {
        // MH_MAGIC_64 and MH_MAGIC, as a little-endian machine writes them.
        0xFEED_FACF | 0xFEED_FACE => {
            let arch = mach_arch(u32_le(bytes, 4)?);
            return Some(BuiltFor { os: Os::Macos, archs: arch.into_iter().collect() });
        }
        _ => {}
    }
    // FAT_MAGIC, big-endian: a count, then one 20-byte record per program,
    // each starting with its CPU type. Java class files share the magic, so
    // a count no universal program would have is not taken for one.
    if u32_be(bytes, 0)? != 0xCAFE_BABE {
        return None;
    }
    let count = u32_be(bytes, 4)?;
    if count == 0 || count > 16 {
        return None;
    }
    let archs = (0..count as usize)
        .filter_map(|i| u32_be(bytes, 8 + i * 20))
        .filter_map(mach_arch)
        .collect();
    Some(BuiltFor { os: Os::Macos, archs })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The first bytes of an ELF program for `machine`, little-endian.
    pub(crate) fn elf_header(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2; // 64-bit
        bytes[5] = 1; // little-endian
        bytes[6] = 1;
        bytes[0x12..0x14].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn pe_header(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x100];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn mach_header(cpu: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; 32];
        bytes[..4].copy_from_slice(&0xFEED_FACFu32.to_le_bytes());
        bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
        bytes
    }

    fn universal(cpus: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(cpus.len()).unwrap().to_be_bytes());
        for cpu in cpus {
            bytes.extend_from_slice(&cpu.to_be_bytes());
            bytes.extend_from_slice(&[0u8; 16]);
        }
        bytes
    }

    fn of(os: Os, archs: &[Arch]) -> BuiltFor {
        BuiltFor { os, archs: archs.to_vec() }
    }

    #[test]
    fn each_platform_xpack_builds_for_is_recognised_by_its_header() {
        assert_eq!(built_for(&pe_header(0x8664)), Some(of(Os::Windows, &[Arch::X64])));
        assert_eq!(built_for(&pe_header(0xAA64)), Some(of(Os::Windows, &[Arch::Arm64])));
        assert_eq!(built_for(&elf_header(0x3E)), Some(of(Os::Linux, &[Arch::X64])));
        assert_eq!(built_for(&elf_header(0xB7)), Some(of(Os::Linux, &[Arch::Arm64])));
        assert_eq!(built_for(&mach_header(CPU_X86_64)), Some(of(Os::Macos, &[Arch::X64])));
        assert_eq!(built_for(&mach_header(CPU_ARM64)), Some(of(Os::Macos, &[Arch::Arm64])));
        assert_eq!(
            built_for(&universal(&[CPU_X86_64, CPU_ARM64])),
            Some(of(Os::Macos, &[Arch::X64, Arch::Arm64]))
        );
    }

    #[test]
    fn a_processor_xpack_does_not_build_for_is_named_as_none() {
        assert_eq!(built_for(&pe_header(0x014C)), Some(of(Os::Windows, &[]))); // i386
        assert_eq!(built_for(&elf_header(0x28)), Some(of(Os::Linux, &[]))); // 32-bit ARM
    }

    #[test]
    fn what_is_not_a_program_is_not_mistaken_for_one() {
        assert_eq!(built_for(b""), None);
        assert_eq!(built_for(b"MZ"), None, "a DOS header with nowhere to point");
        assert_eq!(built_for(b"#!/bin/sh\necho hi\n"), None);
        assert_eq!(built_for(b"a launcher"), None);
        // A Java class file shares the universal magic; its version is no count.
        let class = [0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00, 0x00, 0x41];
        assert_eq!(built_for(&class), None);
        // A PE offset past the end of what was read.
        let mut truncated = pe_header(0x8664);
        truncated.truncate(0x82);
        assert_eq!(built_for(&truncated), None);
    }

    #[test]
    fn a_big_endian_elf_is_read_in_its_own_byte_order() {
        let mut bytes = elf_header(0);
        bytes[5] = 2;
        bytes[0x12..0x14].copy_from_slice(&0xB7u16.to_be_bytes());
        assert_eq!(built_for(&bytes), Some(of(Os::Linux, &[Arch::Arm64])));
    }

    #[test]
    fn a_program_for_another_platform_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xpack-launcher");
        std::fs::write(&path, mach_header(CPU_ARM64)).unwrap();

        let linux = Platform::new(Os::Linux, Arch::X64);
        let err = ensure_built_for(&path, linux, "launcher").unwrap_err().to_string();
        assert!(err.contains("is a macos program") && err.contains("linux-x64"), "{err}");

        let intel_mac = Platform::new(Os::Macos, Arch::X64);
        let err = ensure_built_for(&path, intel_mac, "launcher").unwrap_err().to_string();
        assert!(err.contains("built for arm64") && err.contains("macos-x64"), "{err}");

        assert!(ensure_built_for(&path, Platform::new(Os::Macos, Arch::Arm64), "launcher").is_ok());

        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        let err = ensure_built_for(&path, linux, "launcher").unwrap_err().to_string();
        assert!(err.contains("is not a linux program"), "{err}");
    }

    #[test]
    fn a_universal_program_serves_each_processor_it_carries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xpack-launcher");
        std::fs::write(&path, universal(&[CPU_X86_64, CPU_ARM64])).unwrap();
        for arch in [Arch::X64, Arch::Arm64] {
            assert!(ensure_built_for(&path, Platform::new(Os::Macos, arch), "launcher").is_ok());
        }
    }
}
