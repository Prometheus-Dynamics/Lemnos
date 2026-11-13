//! Layout types: a word, a bit field, and an address register. Every module of the crate uses
//! them; the values come from [`crate::vpu720`], which names the header each comes from.

/// A register word of the block's file: its first word, how many words it spans, and whether
/// the header lets a job write it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Word {
    /// Its word in the file (its byte offset over four).
    pub index: u16,
    /// How many words it spans.
    pub words: u16,
    /// Whether a job may write it.
    pub writable: bool,
}

impl Word {
    /// Builds a layout entry. Used by the generated module.
    #[must_use]
    pub const fn new(index: u16, words: u16, writable: bool) -> Self {
        Self {
            index,
            words,
            writable,
        }
    }
}

/// A bit field: its word, the lowest bit it starts at, its width, whether a job may write it,
/// and the encodings the header lists for it (empty when every value of the width is allowed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// Its word in the file. A field never straddles two words (the header's bit fields do not).
    pub word: u16,
    /// The lowest bit, 0 to 31.
    pub lsb: u8,
    /// Its width in bits, 1 to 32.
    pub width: u8,
    /// Whether the word it sits in may be written.
    pub writable: bool,
    /// The encodings the header lists, when it lists them and marks the rest reserved.
    pub values: &'static [u32],
}

impl Field {
    /// Builds a layout entry. Used by the generated module.
    #[must_use]
    pub const fn new(
        word: u16,
        lsb: u8,
        width: u8,
        writable: bool,
        values: &'static [u32],
    ) -> Self {
        Self {
            word,
            lsb,
            width,
            writable,
            values,
        }
    }

    /// The field's mask, before the shift.
    #[must_use]
    pub const fn mask(self) -> u32 {
        if self.width >= 32 {
            u32::MAX
        } else {
            (1u32 << self.width) - 1
        }
    }
}

/// An address register: a word that holds a buffer's bus address, which the driver writes from
/// the caller's buffer. Its word is the whole register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressReg {
    /// Its word in the file.
    pub word: u16,
    /// The alignment, in bytes, the header gives for the address (1 when it gives none).
    pub align: u32,
}

impl AddressReg {
    /// Builds a layout entry. Used by the generated module.
    #[must_use]
    pub const fn new(word: u16, align: u32) -> Self {
        Self { word, align }
    }
}
