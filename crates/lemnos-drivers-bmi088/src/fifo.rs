//! The BMI088's two FIFOs: the register map and the accelerometer frame
//! parser. The frame layout follows Bosch's BMI08x FIFO application note
//! (BST-MIS-AN005, rev 1.1), sections 3 and 4.
//!
//! A read takes the samples the chip buffered since the last one. The chip
//! stamps nothing: the caller derives each sample's time from the output
//! data rate and the time of the read.

pub(crate) const ACC_FIFO_LENGTH_0: u16 = 0x24;
pub(crate) const ACC_FIFO_DATA: u16 = 0x26;
pub(crate) const ACC_FIFO_DOWNS: u16 = 0x45;
pub(crate) const ACC_FIFO_CONFIG_0: u16 = 0x48;
pub(crate) const ACC_FIFO_CONFIG_1: u16 = 0x49;
pub(crate) const GYR_FIFO_STATUS: u16 = 0x0e;
pub(crate) const GYR_FIFO_CONFIG_0: u16 = 0x3d;
pub(crate) const GYR_FIFO_CONFIG_1: u16 = 0x3e;
pub(crate) const GYR_FIFO_DATA: u16 = 0x3f;

/// Most samples one read returns. The gyroscope's FIFO holds 100 frames and
/// the accelerometer's 1024 bytes (146 frames), so a read of this many covers
/// 120 ms at 400 Hz.
pub const MAX_SAMPLES: usize = 48;
pub(crate) const ACC_FRAME_LEN: usize = 7;
pub(crate) const GYR_FRAME_LEN: usize = 6;

/// The accelerometer frame header: the top six bits are the signature, the
/// bottom two are the INT2 and INT1 tags (ignored here).
const HEADER_MASK: u8 = 0xfc;
/// Data frame: header, then X, Y, Z (6 bytes).
const HEADER_DATA: u8 = 0x84;
/// Skip frame (the FIFO overflowed): header, then the number skipped.
const HEADER_SKIP: u8 = 0x40;
/// Sensor time frame, only when the FIFO runs empty during a read: header,
/// then 3 bytes.
const HEADER_TIME: u8 = 0x44;
/// Configuration change: header, then one byte of what changed.
const HEADER_CONFIG: u8 = 0x48;
/// Sample drop: header, then one ignored byte.
const HEADER_DROP: u8 = 0x50;

/// `ACC_FIFO_CONFIG_0` for STREAM mode (bit 0 clear; bit 1 must be set): the
/// newest samples are kept when the FIFO is full.
pub(crate) const ACC_CONFIG_0_STREAM: u8 = 0x02;
/// `ACC_FIFO_CONFIG_1`: accelerometer data stored (bit 6), bit 4 must be set.
pub(crate) const ACC_CONFIG_1_ACC: u8 = 0x50;
/// `ACC_FIFO_DOWNS`: no down-sampling (bit 7 must be set).
pub(crate) const ACC_DOWNS_NONE: u8 = 0x80;
/// `GYR_FIFO_CONFIG_1`: STREAM mode (the newest frames are kept when full).
pub(crate) const GYR_CONFIG_1_STREAM: u8 = 0x80;

/// What one parse of accelerometer FIFO bytes found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Parsed {
    /// Samples written to the output, oldest first.
    pub samples: usize,
    /// Bytes the frames took (a frame cut short at the end is not taken).
    pub consumed: usize,
    /// Frames the FIFO dropped on overflow (from skip frames).
    pub skipped: u32,
}

/// Parses accelerometer FIFO bytes (oldest first). Data frames become
/// samples; skip, time, configuration and drop frames are stepped over. A
/// frame cut short at the end is left for the next read, as the chip does.
/// Stops when `out` is full.
pub(crate) fn parse_accel(bytes: &[u8], out: &mut [[i16; 3]]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] & HEADER_MASK {
            HEADER_DATA => {
                let end = i + ACC_FRAME_LEN;
                if end > bytes.len() || parsed.samples == out.len() {
                    break;
                }
                out[parsed.samples] = axes(&bytes[i + 1..end]);
                parsed.samples += 1;
                i = end;
            }
            HEADER_SKIP => {
                let Some(&count) = bytes.get(i + 1) else {
                    break;
                };
                parsed.skipped += u32::from(count);
                i += 2;
            }
            HEADER_TIME => {
                if i + 4 > bytes.len() {
                    break;
                }
                i += 4;
            }
            HEADER_CONFIG | HEADER_DROP => {
                if i + 2 > bytes.len() {
                    break;
                }
                i += 2;
            }
            // Not a frame header: step over it.
            _ => i += 1,
        }
    }
    parsed.consumed = i;
    parsed
}

/// Three little-endian 16-bit axes.
pub(crate) fn axes(bytes: &[u8]) -> [i16; 3] {
    let mut out = [0i16; 3];
    let (pairs, _) = bytes.as_chunks::<2>();
    for (value, pair) in out.iter_mut().zip(pairs) {
        *value = i16::from_le_bytes(*pair);
    }
    out
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// A data frame with the given axes.
    fn data(x: i16, y: i16, z: i16) -> [u8; 7] {
        let mut frame = [0u8; 7];
        frame[0] = HEADER_DATA;
        frame[1..3].copy_from_slice(&x.to_le_bytes());
        frame[3..5].copy_from_slice(&y.to_le_bytes());
        frame[5..7].copy_from_slice(&z.to_le_bytes());
        frame
    }

    fn parse(bytes: &[u8], capacity: usize) -> (Parsed, Vec<[i16; 3]>) {
        let mut out = vec![[0i16; 3]; capacity];
        let parsed = parse_accel(bytes, &mut out);
        out.truncate(parsed.samples);
        (parsed, out)
    }

    #[test]
    fn data_frames_are_samples_in_order() {
        let mut bytes = data(1, -2, 3).to_vec();
        bytes.extend_from_slice(&data(-4, 5, -6));
        let (parsed, samples) = parse(&bytes, 4);
        assert_eq!(samples, vec![[1, -2, 3], [-4, 5, -6]]);
        assert_eq!(parsed.consumed, 14);
        assert_eq!(parsed.skipped, 0);
    }

    #[test]
    fn tag_bits_are_ignored() {
        // INT1 and INT2 tags set: header 0x87 is still a data frame.
        let mut bytes = data(7, 8, 9);
        bytes[0] |= 0x03;
        let (parsed, samples) = parse(&bytes, 1);
        assert_eq!(samples, vec![[7, 8, 9]]);
        assert_eq!(parsed.samples, 1);
    }

    #[test]
    fn skip_frames_count_dropped_frames_and_produce_no_sample() {
        let mut bytes = vec![HEADER_SKIP, 5];
        bytes.extend_from_slice(&data(1, 1, 1));
        bytes.extend_from_slice(&[HEADER_SKIP, 0xff]);
        let (parsed, samples) = parse(&bytes, 4);
        assert_eq!(samples, vec![[1, 1, 1]]);
        assert_eq!(parsed.skipped, 5 + 0xff);
        assert_eq!(parsed.consumed, bytes.len());
    }

    #[test]
    fn time_config_and_drop_frames_are_stepped_over() {
        let mut bytes = vec![HEADER_TIME, 1, 2, 3];
        bytes.extend_from_slice(&[HEADER_CONFIG, 0x03]);
        bytes.extend_from_slice(&[HEADER_DROP, 0x00]);
        bytes.extend_from_slice(&data(10, 20, 30));
        let (parsed, samples) = parse(&bytes, 2);
        assert_eq!(samples, vec![[10, 20, 30]]);
        assert_eq!(parsed.consumed, bytes.len());
    }

    #[test]
    fn a_frame_cut_short_is_left_for_the_next_read() {
        let mut bytes = data(1, 2, 3).to_vec();
        bytes.extend_from_slice(&data(4, 5, 6)[..4]);
        let (parsed, samples) = parse(&bytes, 4);
        assert_eq!(samples, vec![[1, 2, 3]]);
        assert_eq!(parsed.consumed, 7);
    }

    #[test]
    fn a_skip_header_cut_short_stops_the_parse() {
        let (parsed, samples) = parse(&[HEADER_SKIP], 4);
        assert!(samples.is_empty());
        assert_eq!(parsed.consumed, 0);
    }

    #[test]
    fn a_full_output_stops_before_the_next_frame() {
        let mut bytes = data(1, 0, 0).to_vec();
        bytes.extend_from_slice(&data(2, 0, 0));
        let (parsed, samples) = parse(&bytes, 1);
        assert_eq!(samples, vec![[1, 0, 0]]);
        assert_eq!(parsed.consumed, 7);
    }

    #[test]
    fn unknown_bytes_are_skipped_one_at_a_time() {
        // 0x00 and 0x01 are not headers; the data frame after them is read.
        let mut bytes = vec![0x00, 0x01];
        bytes.extend_from_slice(&data(9, 9, 9));
        let (parsed, samples) = parse(&bytes, 1);
        assert_eq!(samples, vec![[9, 9, 9]]);
        assert_eq!(parsed.consumed, bytes.len());
    }

    #[test]
    fn an_empty_fifo_is_no_samples() {
        let (parsed, samples) = parse(&[], 4);
        assert!(samples.is_empty());
        assert_eq!(parsed, Parsed::default());
    }
}
