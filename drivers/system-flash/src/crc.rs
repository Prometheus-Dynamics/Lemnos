//! The checksums an xz stream may carry: CRC-32 (the stream and block headers always, and the
//! check of a CRC32 stream) and CRC-64 (the check of a CRC64 stream). Both are the reflected forms
//! xz uses (IEEE 802.3 and ECMA-182); their tables are built at compile time. [`Check`] is the
//! block check a stream names, over the decoded bytes.

use sha2::Digest as _;

const CRC32_POLY: u32 = 0xEDB8_8320;
const CRC64_POLY: u64 = 0xC96C_5795_D787_0F42;

const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ CRC32_POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

const CRC64_TABLE: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u64;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ CRC64_POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// CRC-32 of `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finish()
}

/// An incremental CRC-32.
#[derive(Debug, Clone, Copy)]
pub struct Crc32(u32);

impl Crc32 {
    pub fn new() -> Self {
        Self(!0)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = CRC32_TABLE[((self.0 ^ u32::from(b)) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }

    pub fn finish(self) -> u32 {
        !self.0
    }
}

/// An incremental CRC-64.
#[derive(Debug, Clone, Copy)]
pub struct Crc64(u64);

impl Crc64 {
    pub fn new() -> Self {
        Self(!0)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = CRC64_TABLE[((self.0 ^ u64::from(b)) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }

    pub fn finish(self) -> u64 {
        !self.0
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Crc64 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_check_values() {
        // The check values every CRC catalogue gives for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let mut c = Crc64::new();
        c.update(b"123456789");
        assert_eq!(c.finish(), 0x995D_C9BB_DF19_39FA);
    }

    #[test]
    fn updates_in_pieces_match_one_update() {
        let mut c = Crc32::new();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), crc32(b"123456789"));
    }
}

/// The check an xz block carries over its decoded bytes, of the kind its stream names.
#[derive(Debug, Clone)]
pub enum Check {
    None,
    Crc32(Crc32),
    Crc64(Crc64),
    Sha256(sha2::Sha256),
}

impl Check {
    /// The check for a stream whose flags name `kind` (0, 1, 4 or 10), or `None` for another.
    pub fn of_kind(kind: u8) -> Option<Self> {
        Some(match kind {
            0x00 => Self::None,
            0x01 => Self::Crc32(Crc32::new()),
            0x04 => Self::Crc64(Crc64::new()),
            0x0A => Self::Sha256(sha2::Sha256::new()),
            _ => return None,
        })
    }

    /// The size of the check in the stream.
    pub fn size(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Crc32(_) => 4,
            Self::Crc64(_) => 8,
            Self::Sha256(_) => 32,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::None => {}
            Self::Crc32(c) => c.update(bytes),
            Self::Crc64(c) => c.update(bytes),
            Self::Sha256(h) => sha2::Digest::update(h, bytes),
        }
    }

    /// Whether the decoded bytes so far have the check `stored`, as the stream wrote it.
    pub fn matches(self, stored: &[u8]) -> bool {
        match self {
            Self::None => true,
            Self::Crc32(c) => stored == c.finish().to_le_bytes(),
            Self::Crc64(c) => stored == c.finish().to_le_bytes(),
            Self::Sha256(h) => stored == sha2::Digest::finalize(h).as_slice(),
        }
    }
}
