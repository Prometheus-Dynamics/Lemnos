//! The board run's fixture: a small JPEG written here from the JPEG syntax (ITU-T T.81), and the
//! probe's input buffer `[qtable | mincode | value | stream]` at the offsets in
//! [`lemnos_driver_rk_jpeg::probe`]. No vendor data: every byte is written in this file.
//!
//! The picture is 64 by 50, one grey component, one DC and one AC table with a single one-bit code
//! each. Every block codes as the two bits `00` (DC difference zero, end of block), so the entropy
//! data is 14 zero bytes for 56 blocks.
//!
//! The three tables the probe reads are each allotted [`TABLE_BYTES`] bytes, so their lengths
//! ([`TARGETS`]) share one field value under 4-byte words: 16. The joint sweep needs that.

#![allow(dead_code)] // each helper is used by one of the test binaries that include this module

use lemnos_driver_rk_jpeg::probe::{HMIN_OFF, HVAL_OFF, QTBL_OFF, STREAM_OFF};

/// Picture width in pixels.
pub const WIDTH: u16 = 64;
/// Picture height in pixels.
pub const HEIGHT: u16 = 50;
/// The bytes each table is allotted in the input buffer, and so its length for the probe.
pub const TABLE_BYTES: usize = 64;
/// The three table lengths the board run passes as `RKJPEG_TARGETS`: qtable, mincode, value.
pub const TARGETS: [u32; 3] = [64, 64, 64];
/// The value `RKJPEG_TARGETS` takes on the command line.
pub const TARGETS_ENV: &str = "64,64,64";

/// The quantisation table's payload: 64 values, all 16.
pub const DQT_PAYLOAD: [u8; 64] = [16; 64];
/// The Huffman table's counts: one code, of length one.
pub const COUNTS: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// The Huffman table's symbols: symbol zero, which is both the DC difference of category zero and
/// the AC end of block.
pub const SYMBOLS: [u8; 1] = [0];

/// The JPEG file: SOI, DQT, SOF0, two DHT, SOS, the entropy data, EOI.
#[must_use]
pub fn jpeg() -> Vec<u8> {
    let mut j = vec![0xff, 0xd8];
    // DQT: 8-bit table 0.
    j.extend([0xff, 0xdb, 0x00, 0x43, 0x00]);
    j.extend(DQT_PAYLOAD);
    // SOF0: 8-bit, height, width, one component (id 1, sampling 1x1, table 0).
    j.extend([0xff, 0xc0, 0x00, 0x0b, 8]);
    j.extend(HEIGHT.to_be_bytes());
    j.extend(WIDTH.to_be_bytes());
    j.extend([1, 1, 0x11, 0]);
    // DHT: DC table 0 (class 0), then AC table 0 (class 1).
    j.extend([0xff, 0xc4, 0x00, 0x14, 0x00]);
    j.extend(COUNTS);
    j.extend(SYMBOLS);
    j.extend([0xff, 0xc4, 0x00, 0x14, 0x10]);
    j.extend(COUNTS);
    j.extend(SYMBOLS);
    // SOS: one component (id 1) with DC 0 and AC 0, spectral 0 to 63, no approximation.
    j.extend([0xff, 0xda, 0x00, 0x08, 1, 1, 0x00, 0, 63, 0]);
    j.extend(entropy());
    j.extend([0xff, 0xd9]);
    j
}

/// Blocks in the picture: 8 by 8 blocks, rounded up.
#[must_use]
pub fn blocks() -> usize {
    usize::from(WIDTH).div_ceil(8) * usize::from(HEIGHT).div_ceil(8)
}

/// The entropy data: each block is the two bits `00`, and the bit count is a whole number of bytes.
fn entropy() -> Vec<u8> {
    vec![0; (blocks() * 2).div_ceil(8)]
}

/// The probe's input buffer for [`jpeg`]: each table at its layout offset, and the stream after
/// them at [`STREAM_OFF`].
#[must_use]
pub fn board_input() -> Vec<u8> {
    let jpeg = jpeg();
    let mut input = vec![0u8; STREAM_OFF as usize];
    let qtbl = QTBL_OFF as usize;
    input[qtbl..qtbl + TABLE_BYTES].copy_from_slice(&DQT_PAYLOAD);
    let hmin = HMIN_OFF as usize;
    input[hmin..hmin + COUNTS.len()].copy_from_slice(&COUNTS);
    let hval = HVAL_OFF as usize;
    input[hval..hval + SYMBOLS.len()].copy_from_slice(&SYMBOLS);
    input.extend(jpeg);
    input
}

/// What [`parse_jpeg`] reads from a JPEG file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Width and height.
    pub dims: (u16, u16),
    /// The quantisation table's payload.
    pub dqt: Vec<u8>,
    /// The DC table's counts, one per code length.
    pub dc_counts: Vec<u8>,
    /// The DC table's symbols.
    pub dc_symbols: Vec<u8>,
    /// The number of entropy-coded bytes between SOS and EOI.
    pub entropy_len: usize,
}

/// Reads a JPEG's segments, in the order they come. Errors name the segment that is wrong.
pub fn parse_jpeg(j: &[u8]) -> Result<Parsed, String> {
    if j.get(..2) != Some(&[0xff, 0xd8]) {
        return Err("no SOI".into());
    }
    let mut at = 2;
    let mut out = Parsed {
        dims: (0, 0),
        dqt: Vec::new(),
        dc_counts: Vec::new(),
        dc_symbols: Vec::new(),
        entropy_len: 0,
    };
    loop {
        if j.get(at..at + 2) == Some(&[0xff, 0xd9]) {
            return Ok(out);
        }
        if j.get(at) != Some(&0xff) {
            return Err(format!("no marker at {at}"));
        }
        let marker = *j.get(at + 1).ok_or("truncated marker")?;
        let len = usize::from(u16::from_be_bytes(
            j.get(at + 2..at + 4)
                .ok_or("no length")?
                .try_into()
                .expect("two bytes"),
        ));
        let body = j.get(at + 4..at + 2 + len).ok_or("truncated segment")?;
        match marker {
            0xdb => {
                if body.first() != Some(&0) {
                    return Err("DQT is not 8-bit table 0".into());
                }
                out.dqt = body[1..].to_vec();
            }
            0xc0 => {
                if body.first() != Some(&8) || body.get(5) != Some(&1) {
                    return Err("SOF0 is not 8-bit with one component".into());
                }
                let height = u16::from_be_bytes([body[1], body[2]]);
                let width = u16::from_be_bytes([body[3], body[4]]);
                out.dims = (width, height);
            }
            0xc4 => {
                let class = body.first().ok_or("empty DHT")? >> 4;
                if class == 0 {
                    out.dc_counts = body[1..17].to_vec();
                    let total: usize = out.dc_counts.iter().map(|&c| usize::from(c)).sum();
                    out.dc_symbols = body[17..17 + total].to_vec();
                }
            }
            0xda => {
                // The entropy data runs from here to the EOI at the end.
                let start = at + 2 + len;
                let end = j
                    .len()
                    .checked_sub(2)
                    .filter(|&e| j[e..] == [0xff, 0xd9])
                    .ok_or("no EOI at the end")?;
                out.entropy_len = end - start;
                return Ok(out);
            }
            other => return Err(format!("unexpected marker {other:#x}")),
        }
        at += 2 + len;
    }
}
