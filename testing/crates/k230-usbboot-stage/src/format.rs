//! The wire format the host and the stage share: 60-byte packets, their command and result
//! codes, the boot header, and the CRC-32 the header carries.
//!
//! The packet layout and the command numbers are the burner protocol of the vendor's flash tool
//! (`kburn.h` and `burner_uboot.cpp` at k230_flash 33a75f0; see `docs/flashing.md`). The stage
//! implements a subset of it, as our own code. The boot header's magic and CRC rule are the image
//! header rule of the same tool (`kdimage.h:13`, `kdimage.cpp:168-180`). The rest of the header is
//! ours. Whether the boot ROM checks any of it is UNVERIFIED.

/// A command or a reply: six bytes of header, then up to [`PACKET_DATA`] bytes of data.
pub const PACKET: usize = 60;
/// The header of a packet: command, result (or zero), data size, each a little-endian `u16`.
pub const PACKET_HEAD: usize = 6;
/// The data of a packet: `PACKET - PACKET_HEAD`.
pub const PACKET_DATA: usize = PACKET - PACKET_HEAD;
/// A reply's command is the command with this bit set.
pub const REPLY_FLAG: u16 = 0x8000;
/// A reply's result when the command worked.
pub const RESULT_OK: u16 = 1;
/// A reply's result for a failed command, without a message.
pub const RESULT_ERROR: u16 = 2;
/// A reply's result when the data is a message (the reason, as text).
pub const RESULT_MESSAGE: u16 = 0xff;
/// The reboot command's guard: "Rbot", sent as a little-endian `u64`.
pub const REBOOT_MARK: u64 = 0x5262_6f74;

/// The command numbers (`kburn_pkt_cmd`, burner_uboot.cpp:16-33).
pub mod cmd {
    pub const NOP: u16 = 0x00;
    pub const REBOOT: u16 = 0x01;
    pub const PROBE: u16 = 0x10;
    pub const GET_INFO: u16 = 0x11;
    pub const ERASE: u16 = 0x20;
    pub const WRITE: u16 = 0x21;
    pub const WRITE_CHUNK: u16 = 0x22;
    pub const READ: u16 = 0x23;
    pub const READ_CHUNK: u16 = 0x24;
    pub const VERIFY: u16 = 0x25;
}

/// What the stage answers to the vendor request that asks for the chip's text (wValue 0). It is
/// ours: the host reads it to tell the stage from the boot ROM ("K230") and the vendor's burner.
pub const STAGE_IDENT: &str = "Lemnos stage for K230";
/// The loader version the stage reports (vendor request 0, wValue 1). Version 2 adds VERIFY.
pub const LOADER_VERSION: u32 = 2;
/// The stage's data buffer and the bulk size it reports at PROBE: one write or read piece.
pub const CHUNK: usize = 4096;
/// The first block of a medium: the boot header and its erase unit. The payload starts after it.
pub const HEADER_BLOCK: u64 = 4096;
/// The boot header's length: the first 512 bytes of the header block.
pub const BOOT_HEADER_LEN: usize = 512;
/// The image header's magic (`KDIMG_HADER_MAGIC`, kdimage.h:13).
pub const BOOT_MAGIC: u32 = 0x27cb_8f93;
/// This header's version. A header of another version is not decoded.
pub const BOOT_VERSION: u32 = 1;
/// The medium type the stage reports in GET_INFO (the vendor's SD code, `KBurnMediumType`).
pub const STAGE_MEDIUM_TYPE: u8 = 2;

/// A command packet: the command, a zero result, the data size, then the data.
pub fn packet(command: u16, data: &[u8]) -> [u8; PACKET] {
    debug_assert!(
        data.len() <= PACKET_DATA,
        "a packet holds {PACKET_DATA} bytes of data"
    );
    let mut packet = [0u8; PACKET];
    packet[0..2].copy_from_slice(&command.to_le_bytes());
    packet[4..6].copy_from_slice(&(data.len() as u16).to_le_bytes());
    packet[6..6 + data.len()].copy_from_slice(data);
    packet
}

/// A reply packet: the command with [`REPLY_FLAG`], the result, the data size, then the data.
pub fn reply(command: u16, result: u16, data: &[u8]) -> [u8; PACKET] {
    debug_assert!(
        data.len() <= PACKET_DATA,
        "a reply holds {PACKET_DATA} bytes of data"
    );
    let mut packet = [0u8; PACKET];
    packet[0..2].copy_from_slice(&(command | REPLY_FLAG).to_le_bytes());
    packet[2..4].copy_from_slice(&result.to_le_bytes());
    packet[4..6].copy_from_slice(&(data.len() as u16).to_le_bytes());
    packet[6..6 + data.len()].copy_from_slice(data);
    packet
}

/// A little-endian `u16` at `at`.
pub fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// A little-endian `u32` at `at`.
pub fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// A little-endian `u64` at `at`.
pub fn le64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

/// Writes `value` as a little-endian `u64` at `at`.
pub fn put64(out: &mut [u8], at: usize, value: u64) {
    out[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// Writes `value` as a little-endian `u32` at `at`.
pub fn put32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// CRC-32 (IEEE, reflected), the checksum `kdimage.cpp` computes over a header: `crc32(0, ...)`
/// there is this function. Bitwise, so it needs no table. Checked against the standard check value
/// in the tests.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// True when `bytes` holds a header whose magic and CRC are right. This is the weakest test a boot
/// ROM could apply, so the simulator's ROM uses it: the stage's guards use it too, to protect any
/// header the ROM might boot. Whether the ROM applies more is UNVERIFIED.
pub fn rom_accepts(bytes: &[u8]) -> bool {
    if bytes.len() < BOOT_HEADER_LEN {
        return false;
    }
    if le32(bytes, 0) != BOOT_MAGIC {
        return false;
    }
    let mut copy = [0u8; BOOT_HEADER_LEN];
    copy.copy_from_slice(&bytes[..BOOT_HEADER_LEN]);
    copy[4..8].fill(0);
    crc32(&copy) == le32(bytes, 4)
}

/// The boot header: which payload the medium holds, and its digest. The header is written last,
/// so a valid header means the payload it names is complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootHeader {
    /// Where the payload starts on the medium.
    pub payload_offset: u64,
    /// The payload's length in bytes (without the padding to a block).
    pub payload_size: u64,
    /// SHA-256 of the payload's `payload_size` bytes.
    pub digest: [u8; 32],
}

impl BootHeader {
    /// The header's 512 bytes. Layout: magic `[0..4]`, CRC `[4..8]` (over the whole 512 bytes with
    /// this field zero), flags `[8..12]` (zero), version `[12..16]`, payload offset `[16..24]`,
    /// payload size `[24..32]`, digest `[32..64]`; the rest zero.
    pub fn encode(&self) -> [u8; BOOT_HEADER_LEN] {
        let mut header = [0u8; BOOT_HEADER_LEN];
        put32(&mut header, 0, BOOT_MAGIC);
        put32(&mut header, 12, BOOT_VERSION);
        put64(&mut header, 16, self.payload_offset);
        put64(&mut header, 24, self.payload_size);
        header[32..64].copy_from_slice(&self.digest);
        let crc = crc32(&header);
        put32(&mut header, 4, crc);
        header
    }

    /// Decodes a header: the magic and CRC must be right and the version must be this one.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if !rom_accepts(bytes) || le32(bytes, 12) != BOOT_VERSION {
            return None;
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&bytes[32..64]);
        Some(Self {
            payload_offset: le64(bytes, 16),
            payload_size: le64(bytes, 24),
            digest,
        })
    }
}
