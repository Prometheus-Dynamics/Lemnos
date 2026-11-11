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

const WIDE: bool = cfg!(target_pointer_width = "64");
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
struct File<'a>(&'a [u8]);

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
        if WIDE {
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

const LAYOUT: Layout = if WIDE {
    Layout {
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
    }
} else {
    Layout {
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
    }
};

/// Checks the file and returns the head of its package descriptor.
pub(crate) fn inspect(bytes: &[u8]) -> Result<Head, Refusal> {
    let l = &LAYOUT;
    let file = File(bytes);
    if bytes.len() < 4 || bytes[..4] != *b"\x7fELF" {
        return Err(not_a_package("not a shared object"));
    }
    let class = if WIDE { 2 } else { 1 };
    let order = if cfg!(target_endian = "little") { 1 } else { 2 };
    let ident: [u8; 6] = file.array(0)?;
    if ident[4] != class || ident[5] != order {
        return Err(not_a_package("built for another kind of machine"));
    }
    if file.u16(16)? != ET_DYN {
        return Err(not_a_package("not a shared object"));
    }
    if MACHINE.is_some_and(|machine| file.u16(18).is_ok_and(|built_for| built_for != machine)) {
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
