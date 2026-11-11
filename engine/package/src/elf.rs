//! Reads, from a file's bytes alone, whether it is a whole shared object for this machine that
//! exports the package entry, and what that entry's first fields say. Nothing is run and nothing
//! is mapped: a file that fails here never reaches the dynamic loader.

use lemnos_abi::AbiVersion;
use lemnos_abi::package::{DESCRIPTOR_HEAD, PACKAGE_ENTRY};

use crate::Refusal;

/// The start of a package's descriptor, as its file holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Head {
    pub(crate) magic: u64,
    pub(crate) version: AbiVersion,
}

/// The kind of machine a file must be built for: its word size and its number in a file's
/// header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Machine {
    /// 64-bit files. Otherwise 32-bit ones, where every address-sized field is half as wide
    /// and the tables are laid out differently.
    pub(crate) wide: bool,
    /// `None` where this build does not know its own number: any is taken.
    pub(crate) number: Option<u16>,
}

/// The machine this host runs on.
pub(crate) const HOST: Machine = Machine {
    wide: cfg!(target_pointer_width = "64"),
    number: MACHINE,
};

const ET_DYN: u16 = 3;
/// This machine's number in a file's header, where this build knows it.
const MACHINE: Option<u16> = if cfg!(target_arch = "x86_64") {
    Some(62)
} else if cfg!(target_arch = "aarch64") {
    Some(183)
} else if cfg!(target_arch = "arm") {
    Some(40)
} else if cfg!(target_arch = "x86") {
    Some(3)
} else if cfg!(any(target_arch = "riscv32", target_arch = "riscv64")) {
    Some(243)
} else {
    None
};
const PT_LOAD: u32 = 1;
const SHT_DYNSYM: u32 = 11;
const SHT_NOBITS: u32 = 8;

fn not_a_package(why: &'static str) -> Refusal {
    Refusal::NotAPackage(why)
}

/// A reader over the file that turns "past the end" into [`Refusal::Truncated`].
struct File<'a>(&'a [u8], bool);

impl<'a> File<'a> {
    fn bytes(&self, at: u64, len: u64) -> Result<&'a [u8], Refusal> {
        let start = usize::try_from(at).map_err(|_| Refusal::Truncated)?;
        let len = usize::try_from(len).map_err(|_| Refusal::Truncated)?;
        let end = start.checked_add(len).ok_or(Refusal::Truncated)?;
        self.0.get(start..end).ok_or(Refusal::Truncated)
    }

    fn array<const N: usize>(&self, at: u64) -> Result<[u8; N], Refusal> {
        let mut out = [0; N];
        out.copy_from_slice(self.bytes(at, N as u64)?);
        Ok(out)
    }

    fn u16(&self, at: u64) -> Result<u16, Refusal> {
        self.array(at).map(u16::from_ne_bytes)
    }

    fn u32(&self, at: u64) -> Result<u32, Refusal> {
        self.array(at).map(u32::from_ne_bytes)
    }

    fn u64(&self, at: u64) -> Result<u64, Refusal> {
        self.array(at).map(u64::from_ne_bytes)
    }

    /// An address-sized field: 8 bytes in a 64-bit file, 4 in a 32-bit one.
    fn word(&self, at: u64) -> Result<u64, Refusal> {
        if self.1 {
            self.u64(at)
        } else {
            self.u32(at).map(u64::from)
        }
    }
}

/// Where each field sits, for this machine's word size.
struct Layout {
    phoff: u64,
    shoff: u64,
    phentsize: u64,
    phnum: u64,
    shentsize: u64,
    shnum: u64,
    p_offset: u64,
    p_vaddr: u64,
    p_filesz: u64,
    sh_offset: u64,
    sh_size: u64,
    sh_link: u64,
    sh_entsize: u64,
    st_shndx: u64,
    st_value: u64,
    st_size: u64,
    sym_size: u64,
}

/// The layout of a 64-bit file.
const WIDE: Layout = Layout {
    phoff: 32,
    shoff: 40,
    phentsize: 54,
    phnum: 56,
    shentsize: 58,
    shnum: 60,
    p_offset: 8,
    p_vaddr: 16,
    p_filesz: 32,
    sh_offset: 24,
    sh_size: 32,
    sh_link: 40,
    sh_entsize: 56,
    st_shndx: 6,
    st_value: 8,
    st_size: 16,
    sym_size: 24,
};

/// The layout of a 32-bit file.
const NARROW: Layout = Layout {
    phoff: 28,
    shoff: 32,
    phentsize: 42,
    phnum: 44,
    shentsize: 46,
    shnum: 48,
    p_offset: 4,
    p_vaddr: 8,
    p_filesz: 16,
    sh_offset: 16,
    sh_size: 20,
    sh_link: 24,
    sh_entsize: 36,
    st_shndx: 14,
    st_value: 4,
    st_size: 8,
    sym_size: 16,
};

/// Checks the file against this host's machine and returns the head of its package
/// descriptor.
pub(crate) fn inspect(bytes: &[u8]) -> Result<Head, Refusal> {
    inspect_for(bytes, HOST)
}

/// [`inspect`], for a file that must be built for `machine`.
pub(crate) fn inspect_for(bytes: &[u8], machine: Machine) -> Result<Head, Refusal> {
    let l = if machine.wide { &WIDE } else { &NARROW };
    let file = File(bytes, machine.wide);
    if bytes.len() < 4 || bytes[..4] != *b"\x7fELF" {
        return Err(not_a_package("not a shared object"));
    }
    let class = if machine.wide { 2 } else { 1 };
    let order = if cfg!(target_endian = "little") { 1 } else { 2 };
    let ident: [u8; 6] = file.array(0)?;
    if ident[4] != class || ident[5] != order {
        return Err(not_a_package("built for another kind of machine"));
    }
    if file.u16(16)? != ET_DYN {
        return Err(not_a_package("not a shared object"));
    }
    if machine
        .number
        .is_some_and(|number| file.u16(18).is_ok_and(|built_for| built_for != number))
    {
        return Err(not_a_package("built for another kind of machine"));
    }

    let (phoff, shoff) = (file.word(l.phoff)?, file.word(l.shoff)?);
    let (phentsize, phnum) = (
        u64::from(file.u16(l.phentsize)?),
        u64::from(file.u16(l.phnum)?),
    );
    let (shentsize, shnum) = (
        u64::from(file.u16(l.shentsize)?),
        u64::from(file.u16(l.shnum)?),
    );
    // Every table and everything a table points to must be inside the file.
    file.bytes(phoff, phentsize * phnum)?;
    file.bytes(shoff, shentsize * shnum)?;
    let mut loads = Vec::new();
    for index in 0..phnum {
        let at = phoff + index * phentsize;
        if file.u32(at)? != PT_LOAD {
            continue;
        }
        let (offset, vaddr, size) = (
            file.word(at + l.p_offset)?,
            file.word(at + l.p_vaddr)?,
            file.word(at + l.p_filesz)?,
        );
        file.bytes(offset, size)?;
        loads.push((offset, vaddr, size));
    }
    let section = |index: u64| -> Result<(u32, u64, u64, u64), Refusal> {
        if index >= shnum {
            return Err(not_a_package("its section table is broken"));
        }
        let at = shoff + index * shentsize;
        let kind = file.u32(at + 4)?;
        let (offset, size) = (file.word(at + l.sh_offset)?, file.word(at + l.sh_size)?);
        if kind != SHT_NOBITS {
            file.bytes(offset, size)?;
        }
        Ok((kind, offset, size, at))
    };
    let mut symbols = None;
    for index in 0..shnum {
        let (kind, offset, size, at) = section(index)?;
        if kind == SHT_DYNSYM {
            symbols = Some((offset, size, at));
        }
    }
    let Some((offset, size, at)) = symbols else {
        return Err(not_a_package("it exports nothing"));
    };
    let entsize = file.word(at + l.sh_entsize)?;
    if entsize < l.sym_size {
        return Err(not_a_package("its symbol table is broken"));
    }
    let (_, names, names_size, _) = section(u64::from(file.u32(at + l.sh_link)?))?;
    let names = file.bytes(names, names_size)?;
    let wanted = PACKAGE_ENTRY.to_bytes_with_nul();

    for index in 0..size / entsize {
        let at = offset + index * entsize;
        let name = file.u32(at)? as usize;
        if names
            .get(name..)
            .is_none_or(|name| !name.starts_with(wanted))
        {
            continue;
        }
        // Section 0 means the symbol is only referred to here, not defined.
        if file.u16(at + l.st_shndx)? == 0 {
            continue;
        }
        let (value, symbol_size) = (file.word(at + l.st_value)?, file.word(at + l.st_size)?);
        if symbol_size < DESCRIPTOR_HEAD as u64 {
            return Err(not_a_package("its entry is too small to be a descriptor"));
        }
        let held = loads.iter().find(|(_, vaddr, size)| {
            value >= *vaddr && value - vaddr + DESCRIPTOR_HEAD as u64 <= *size
        });
        let Some((offset, vaddr, _)) = held else {
            return Err(not_a_package("its entry is not in the file"));
        };
        let at = offset + (value - vaddr);
        return Ok(Head {
            magic: file.u64(at)?,
            version: AbiVersion {
                struct_size: file.u32(at + 8)?,
                major: file.u16(at + 12)?,
                minor: file.u16(at + 14)?,
                layout: file.u64(at + 16)?,
            },
        });
    }
    Err(not_a_package("it has no package entry"))
}

#[cfg(test)]
mod tests {
    use lemnos_abi::package::PACKAGE_MAGIC;

    use super::*;

    /// A 32-bit ARM file, and a 64-bit x86 one: a machine of each word size.
    const NARROW_ARM: Machine = Machine {
        wide: false,
        number: Some(40),
    };
    const WIDE_X86: Machine = Machine {
        wide: true,
        number: Some(62),
    };

    /// Builds a file field by field from the format's own layout (the ELF specification's
    /// `Elf32_*` and `Elf64_*` structures), not from this module's tables.
    struct Crafted {
        wide: bool,
        bytes: Vec<u8>,
    }

    impl Crafted {
        fn put(&mut self, at: usize, value: &[u8]) {
            if self.bytes.len() < at + value.len() {
                self.bytes.resize(at + value.len(), 0);
            }
            self.bytes[at..at + value.len()].copy_from_slice(value);
        }

        fn u16(&mut self, at: usize, value: u16) {
            self.put(at, &value.to_ne_bytes());
        }

        fn u32(&mut self, at: usize, value: u32) {
            self.put(at, &value.to_ne_bytes());
        }

        fn u64(&mut self, at: usize, value: u64) {
            self.put(at, &value.to_ne_bytes());
        }

        /// An address-sized field.
        fn word(&mut self, at: usize, value: u64) {
            if self.wide {
                self.u64(at, value);
            } else {
                self.u32(at, value as u32);
            }
        }
    }

    /// Where the crafted file keeps things.
    const SEGMENT: usize = 0x200;
    const SYMBOLS: usize = 0x300;
    const NAMES: usize = 0x380;
    const SECTIONS: usize = 0x400;
    /// The segment is mapped here, so an address is not a file offset.
    const MAPPED_AT: u64 = 0x1_0000;
    const DESCRIPTOR_IN_SEGMENT: u64 = 0x20;

    /// A shared object with one loaded segment that holds a descriptor, a dynamic symbol table
    /// that names it `lemnos_package`, and the string table for it.
    fn crafted(wide: bool, machine: u16) -> Vec<u8> {
        let mut file = Crafted {
            wide,
            bytes: Vec::new(),
        };
        let order = if cfg!(target_endian = "little") { 1 } else { 2 };
        file.put(
            0,
            &[0x7f, b'E', b'L', b'F', if wide { 2 } else { 1 }, order, 1],
        );
        file.u16(16, 3); // e_type: ET_DYN
        file.u16(18, machine);
        file.u32(20, 1);
        // e_phoff, e_shoff, then the sizes and counts of both tables.
        let (header, phentsize, shentsize) = if wide { (64, 56, 64) } else { (52, 32, 40) };
        if wide {
            file.u64(32, header as u64);
            file.u64(40, SECTIONS as u64);
            file.u16(52, header as u16);
            file.u16(54, phentsize);
            file.u16(56, 1);
            file.u16(58, shentsize);
            file.u16(60, 3);
        } else {
            file.u32(28, header as u32);
            file.u32(32, SECTIONS as u32);
            file.u16(40, header as u16);
            file.u16(42, phentsize);
            file.u16(44, 1);
            file.u16(46, shentsize);
            file.u16(48, 3);
        }
        // The one program header: PT_LOAD of 0x100 bytes at SEGMENT, mapped at MAPPED_AT.
        file.u32(header, 1);
        if wide {
            file.u64(header + 8, SEGMENT as u64); // p_offset
            file.u64(header + 16, MAPPED_AT); // p_vaddr
            file.u64(header + 32, 0x100); // p_filesz
            file.u64(header + 40, 0x100); // p_memsz
        } else {
            file.u32(header + 4, SEGMENT as u32);
            file.u32(header + 8, MAPPED_AT as u32);
            file.u32(header + 16, 0x100);
            file.u32(header + 20, 0x100);
        }
        // The descriptor's head, inside the segment.
        let descriptor = SEGMENT + DESCRIPTOR_IN_SEGMENT as usize;
        file.u64(descriptor, PACKAGE_MAGIC);
        file.u32(descriptor + 8, 16);
        file.u16(descriptor + 12, 7);
        file.u16(descriptor + 14, 3);
        file.u64(descriptor + 16, 0x0123_4567_89ab_cdef);
        // The names: an empty one, another symbol's, then the entry's.
        let names = b"\0other\0lemnos_package\0";
        file.put(NAMES, names);
        // Two symbols after the null one: `other`, undefined, and the entry.
        let symbol = if wide { 24 } else { 16 };
        let (other, entry) = (SYMBOLS + symbol, SYMBOLS + 2 * symbol);
        file.u32(other, 1);
        file.u32(entry, 7);
        if wide {
            file.u16(entry + 6, 1); // st_shndx
            file.u64(entry + 8, MAPPED_AT + DESCRIPTOR_IN_SEGMENT); // st_value
            file.u64(entry + 16, 32); // st_size
        } else {
            file.u32(entry + 4, (MAPPED_AT + DESCRIPTOR_IN_SEGMENT) as u32);
            file.u32(entry + 8, 28);
            file.u16(entry + 14, 1);
        }
        // Sections: the null one, the dynamic symbols (type 11, linked to section 2), the names.
        let section = |index: usize| SECTIONS + index * shentsize as usize;
        let (symbols, strings) = (section(1), section(2));
        file.u32(symbols + 4, 11);
        file.u32(strings + 4, 3);
        if wide {
            file.u64(symbols + 24, SYMBOLS as u64); // sh_offset
            file.u64(symbols + 32, 3 * symbol as u64); // sh_size
            file.u32(symbols + 40, 2); // sh_link
            file.u64(symbols + 56, symbol as u64); // sh_entsize
            file.u64(strings + 24, NAMES as u64);
            file.u64(strings + 32, names.len() as u64);
        } else {
            file.u32(symbols + 16, SYMBOLS as u32);
            file.u32(symbols + 20, 3 * symbol as u32);
            file.u32(symbols + 24, 2);
            file.u32(symbols + 36, symbol as u32);
            file.u32(strings + 16, NAMES as u32);
            file.u32(strings + 20, names.len() as u32);
        }
        // Up to the end of the section table.
        file.word(section(3) - if wide { 8 } else { 4 }, 0);
        file.bytes
    }

    fn crafted_head() -> Head {
        Head {
            magic: PACKAGE_MAGIC,
            version: AbiVersion {
                struct_size: 16,
                major: 7,
                minor: 3,
                layout: 0x0123_4567_89ab_cdef,
            },
        }
    }

    #[test]
    fn a_32_bit_file_is_read_by_its_own_layout() {
        let file = crafted(false, 40);
        assert_eq!(file.len(), SECTIONS + 3 * 40);
        assert_eq!(inspect_for(&file, NARROW_ARM), Ok(crafted_head()));
        // The same builder makes a 64-bit file the 64-bit branch reads, which real packages
        // already prove right: the builder and the reader agree on both layouts.
        let wide = crafted(true, 62);
        assert_eq!(inspect_for(&wide, WIDE_X86), Ok(crafted_head()));
    }

    #[test]
    fn a_file_of_the_other_word_size_or_machine_is_refused() {
        let other = Refusal::NotAPackage("built for another kind of machine");
        assert_eq!(
            inspect_for(&crafted(false, 40), WIDE_X86),
            Err(other.clone())
        );
        assert_eq!(
            inspect_for(&crafted(true, 62), NARROW_ARM),
            Err(other.clone())
        );
        // The right word size for another processor.
        assert_eq!(inspect_for(&crafted(false, 3), NARROW_ARM), Err(other));
        // A host that does not know its own number takes any.
        let any = Machine {
            wide: false,
            number: None,
        };
        assert_eq!(inspect_for(&crafted(false, 3), any), Ok(crafted_head()));
    }

    #[test]
    fn a_32_bit_file_cut_short_or_pointing_outside_itself_is_refused() {
        let file = crafted(false, 40);
        // Cut anywhere, it is truncated or no longer a shared object: never read past its end.
        for len in 0..file.len() {
            let refused = inspect_for(&file[..len], NARROW_ARM);
            assert!(
                matches!(refused, Err(Refusal::Truncated | Refusal::NotAPackage(_))),
                "cut at {len}: {refused:?}"
            );
        }
        let put = |at: usize, value: u32| {
            let mut changed = file.clone();
            changed[at..at + 4].copy_from_slice(&value.to_ne_bytes());
            inspect_for(&changed, NARROW_ARM)
        };
        // The segment's size in the file runs past the end.
        assert_eq!(put(52 + 16, 0x10_0000), Err(Refusal::Truncated));
        // The section table is somewhere else entirely.
        assert_eq!(put(32, 0x7fff_0000), Err(Refusal::Truncated));
        // The symbol table's size runs past the end.
        assert_eq!(put(SECTIONS + 40 + 20, 0x10_0000), Err(Refusal::Truncated));
        // The entry's address is in no segment of the file.
        let entry = SYMBOLS + 2 * 16;
        assert_eq!(
            put(entry + 4, 0x9000_0000),
            Err(Refusal::NotAPackage("its entry is not in the file"))
        );
        // The entry is too small to hold a descriptor's head.
        assert_eq!(
            put(entry + 8, 8),
            Err(Refusal::NotAPackage(
                "its entry is too small to be a descriptor"
            ))
        );
        // Symbols smaller than the format's.
        assert_eq!(
            put(SECTIONS + 40 + 36, 8),
            Err(Refusal::NotAPackage("its symbol table is broken"))
        );
        // The symbol table links to a section that is not there.
        assert_eq!(
            put(SECTIONS + 40 + 24, 9),
            Err(Refusal::NotAPackage("its section table is broken"))
        );
        // The entry is only referred to, not defined.
        let mut undefined = file.clone();
        undefined[entry + 14..entry + 16].copy_from_slice(&0u16.to_ne_bytes());
        assert_eq!(
            inspect_for(&undefined, NARROW_ARM),
            Err(Refusal::NotAPackage("it has no package entry"))
        );
    }

    /// A package's descriptor in C, from the boundary's header, with nothing of the C library.
    const DESCRIPTOR_C: &str = r#"
#include "lemnos/package.h"
static LemnosStatus register_driver(LemnosSlice *driver) {
    (void)driver;
    return -1;
}
const LemnosPackageDescriptor lemnos_package = {
    LEMNOS_PACKAGE_MAGIC,
    { sizeof(LemnosAbiVersion), LEMNOS_PACKAGE_MAJOR, LEMNOS_PACKAGE_MINOR,
      0x1122334455667788ULL },
    register_driver,
};
"#;

    /// [`DESCRIPTOR_C`] built by the real compiler and linker as a 32-bit shared object, where
    /// this machine's compiler can make one. `None` where it can't.
    fn built_32_bit(dir: &std::path::Path) -> Option<Vec<u8>> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let (source, out) = (dir.join("narrow.c"), dir.join("narrow.so"));
        std::fs::write(&source, DESCRIPTOR_C).ok()?;
        let built = std::process::Command::new("cc")
            .args(["-m32", "-std=c11", "-ffreestanding", "-nostdlib"])
            .args(["-shared", "-fPIC", "-I"])
            .arg(manifest.join("../../include"))
            .arg(&source)
            .arg("-o")
            .arg(&out)
            .output()
            .ok()?;
        if !built.status.success() {
            return None;
        }
        std::fs::read(&out).ok()
    }

    #[test]
    fn a_32_bit_package_from_the_real_linker_is_read() {
        let dir = std::env::temp_dir().join(format!("lemnos-package-elf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let built = built_32_bit(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let Some(file) = built else {
            // No 32-bit compiler here: the crafted files above are the check.
            assert!(
                std::env::var_os("LEMNOS_REQUIRE_M32").is_none(),
                "cc -m32 made no shared object"
            );
            return;
        };
        assert_eq!(file[4], 1, "a 32-bit file");
        let narrow_x86 = Machine {
            wide: false,
            number: Some(3),
        };
        let head = inspect_for(&file, narrow_x86).unwrap();
        assert_eq!(head.magic, PACKAGE_MAGIC);
        let host = lemnos_abi::package::PACKAGE_VERSION;
        assert_eq!(
            (
                head.version.struct_size,
                head.version.major,
                head.version.minor,
                head.version.layout
            ),
            (16, host.major, host.minor, 0x1122_3344_5566_7788)
        );
        // This host, being 64-bit, takes it for another machine's.
        if HOST.wide {
            assert_eq!(
                inspect(&file),
                Err(Refusal::NotAPackage("built for another kind of machine"))
            );
        }
        // Cut short, it is refused like any other.
        assert!(inspect_for(&file[..file.len() / 2], narrow_x86).is_err());
    }
}
