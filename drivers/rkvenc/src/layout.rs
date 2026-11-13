//! Register layouts: where a register sits, which bits a field occupies, and whether a caller may
//! write it. Each value comes from the vendor header its module names (see [`crate::vepu541_h264`]).
//! Nothing here knows what a register is for.

/// The register window a register belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// The write side: what a job sets, from word 0.
    Set,
    /// The read-back side: what a finished job reports (status, stream length, statistics).
    Ret,
    /// The second window, which the write side's job does not reach.
    L2,
    /// A register class of a block that numbers its registers absolutely (the VEPU580 block). The
    /// index is the kernel's `RKVENC_CLASS_TYPE` value: 0 BASE, 1 PIC, 2 RC, 3 PAR, 4 SQI, 5 SCL,
    /// 6 OSD, 7 ST, 8 DEBUG.
    Class(u8),
}

/// A register class: the words it spans in its block, and whether a caller may write them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Class {
    /// The window its registers are in.
    pub region: Region,
    /// Its first word in the block.
    pub first: u16,
    /// How many words it spans, from `first`.
    pub words: u16,
    /// Whether the registers in it may be written.
    pub writable: bool,
}

/// What the header says a register does on access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read only: never writable.
    ReadOnly,
    /// Read and write.
    ReadWrite,
    /// Write only.
    WriteOnly,
    /// Read and write; a written one clears the bit.
    WriteOneToClear,
    /// The header gives no access type for it: never written.
    Unknown,
}

/// A register: its window, its word offset in that window, its size in words, and its alignment
/// note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Register {
    /// The window it is in.
    pub region: Region,
    /// The name in the header's struct.
    pub name: &'static str,
    /// Its first word in the window.
    pub word: u16,
    /// Its size in words, as the register map places the next register.
    pub words: u16,
    /// Its access type.
    pub access: Access,
    /// The alignment the header gives for an address held in it, in bytes; 1 when none.
    pub align: u32,
}

impl Register {
    /// Builds a layout entry. Used by the generated modules.
    #[must_use]
    pub const fn new(
        region: Region,
        name: &'static str,
        word: u16,
        words: u16,
        access: Access,
        align: u32,
    ) -> Self {
        Self {
            region,
            name,
            word,
            words,
            access,
            align,
        }
    }

    /// The register's byte offset in its window.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.word as u32 * 4
    }
}

/// A bit field: the word and the lowest bit it starts at, its width, its sign and whether a
/// caller may write it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// The window it is in.
    pub region: Region,
    /// Its word in the window. A field never straddles two words (the header's bitfields do not).
    pub word: u16,
    /// The lowest bit, 0 to 31.
    pub lsb: u8,
    /// Its width in bits, 1 to 32.
    pub width: u8,
    /// Whether the header declares it signed.
    pub signed: bool,
    /// Whether the register it sits in may be written.
    pub writable: bool,
    /// The encodings the header lists for the field, when it lists them and says the others are
    /// reserved. Empty when every value of the width is allowed.
    pub values: &'static [u32],
}

impl Field {
    /// Builds a layout entry. Used by the generated modules.
    #[must_use]
    pub const fn new(
        region: Region,
        word: u16,
        lsb: u8,
        width: u8,
        signed: bool,
        writable: bool,
        values: &'static [u32],
    ) -> Self {
        Self {
            region,
            word,
            lsb,
            width,
            signed,
            writable,
            values,
        }
    }

    /// The field's mask after the shift.
    #[must_use]
    pub const fn mask(self) -> u32 {
        if self.width >= 32 {
            u32::MAX
        } else {
            (1u32 << self.width) - 1
        }
    }

    /// The field's bits read from `window` (in `region`, starting at word `first`), unsigned.
    /// `None` when the field is in another window or outside this one.
    #[must_use]
    pub fn read(self, region: Region, window: &[u32], first: u16) -> Option<u32> {
        if self.region != region || self.word < first {
            return None;
        }
        let index = usize::from(self.word - first);
        let word = *window.get(index)?;
        Some((word >> self.lsb) & self.mask())
    }
}

/// An address register: a word that holds a buffer's bus address, which the driver writes from
/// the caller's buffer. Its word is the whole register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressReg {
    /// The window it is in.
    pub region: Region,
    /// Its word in the window.
    pub word: u16,
    /// The alignment, in bytes, the header gives for the address (1 when it gives none).
    pub align: u32,
}

impl AddressReg {
    /// Builds a layout entry. Used by the generated modules.
    #[must_use]
    pub const fn new(region: Region, word: u16, align: u32) -> Self {
        Self {
            region,
            word,
            align,
        }
    }
}
