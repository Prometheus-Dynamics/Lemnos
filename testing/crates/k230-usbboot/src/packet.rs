//! The burner's 60-byte packets: the layout and the numbers shared with the stage (see
//! `lemnos_k230_usbboot_stage::format`, which holds them).
//!
//! A command and a reply are both whole packets: a little-endian `cmd`, `result` and `data_size`
//! (`u16` each), then 54 bytes of data. A reply's `cmd` is the command's with 0x8000 set.

pub use lemnos_k230_usbboot_stage::format::{
    CHUNK, PACKET, PACKET_DATA as DATA, REBOOT_MARK, REPLY_FLAG, RESULT_ERROR, RESULT_MESSAGE,
    RESULT_OK, STAGE_IDENT, cmd, le16, le32, le64, packet, reply,
};

/// Stale replies skipped before a command fails (`KBURN_MAX_STALE_RESPONSES`, burner_uboot.cpp:14).
pub const STALE_REPLIES: usize = 8;

/// Little-endian `u64` words, the layout of every command's data.
pub fn words(values: &[u64]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_is_sixty_bytes_with_the_data_after_the_header() {
        let bytes = packet(cmd::PROBE, &[2, 0xff]);
        assert_eq!(bytes.len(), PACKET);
        assert_eq!(&bytes[..6], &[0x10, 0, 0, 0, 2, 0]);
        assert_eq!(&bytes[6..8], &[2, 0xff]);
        assert!(bytes[8..].iter().all(|&byte| byte == 0));
    }
}
