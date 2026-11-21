//! Where a compressed stream or an archive really ends, read from its own structure.

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress};

/// A gzip member as read: where it ends and what its trailer says against what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gzip {
    /// The offset just past the member's 8-byte trailer.
    pub(crate) end: usize,
    /// The decompressed length.
    pub(crate) plain_len: u64,
    /// The trailer's ISIZE.
    pub(crate) isize: u32,
    /// Whether the trailer's CRC-32 matches the decompressed bytes.
    pub(crate) crc_ok: bool,
    /// The first 64 decompressed bytes (fewer when the output is shorter).
    pub(crate) head: Vec<u8>,
}

const FHCRC: u8 = 0x02;
const FEXTRA: u8 = 0x04;
const FNAME: u8 = 0x08;
const FCOMMENT: u8 = 0x10;

/// The CRC-32 (IEEE, reflected) table.
const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

fn crc_update(crc: u32, bytes: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in bytes {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// Reads the gzip member at the start of `data` to its end.
pub(crate) fn gzip(data: &[u8]) -> Result<Gzip, String> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b {
        return Err("not gzip".to_string());
    }
    if data[2] != 8 {
        return Err(format!("gzip method {} is not deflate", data[2]));
    }
    let flags = data[3];
    let mut at = 10usize;
    let short = || "the gzip header runs past the end of the file".to_string();
    if flags & FEXTRA != 0 {
        let n = usize::from(u16::from_le_bytes([
            *data.get(at).ok_or_else(short)?,
            *data.get(at + 1).ok_or_else(short)?,
        ]));
        at += 2 + n;
    }
    for flag in [FNAME, FCOMMENT] {
        if flags & flag != 0 {
            let rest = data.get(at..).ok_or_else(short)?;
            let nul = rest.iter().position(|&b| b == 0).ok_or_else(short)?;
            at += nul + 1;
        }
    }
    if flags & FHCRC != 0 {
        at += 2;
    }
    let mut input = data.get(at..).ok_or_else(short)?;
    let mut d = Box::<DecompressorOxide>::default();
    let mut ring = vec![0u8; 1 << 15];
    let mask = ring.len() - 1;
    let mut pos = 0usize;
    let mut plain_len = 0u64;
    let mut crc = 0u32;
    let mut head = Vec::with_capacity(64);
    loop {
        let (status, used, wrote) = decompress(&mut d, input, &mut ring, pos, 0);
        let mut left = wrote;
        let mut p = pos;
        while left > 0 {
            let run = left.min(ring.len() - p);
            let bytes = &ring[p..p + run];
            crc = crc_update(crc, bytes);
            if head.len() < 64 {
                let take = (64 - head.len()).min(run);
                head.extend_from_slice(&bytes[..take]);
            }
            p = (p + run) & mask;
            left -= run;
        }
        plain_len += wrote as u64;
        pos = (pos + wrote) & mask;
        input = &input[used..];
        at += used;
        match status {
            TINFLStatus::Done => break,
            TINFLStatus::HasMoreOutput => {}
            TINFLStatus::NeedsMoreInput | TINFLStatus::FailedCannotMakeProgress => {
                return Err(format!(
                    "the deflate stream has no end mark: the file ends at byte {} inside it",
                    data.len()
                ));
            }
            other => return Err(format!("the deflate stream is broken ({other:?})")),
        }
    }
    let trailer = data
        .get(at..at + 8)
        .ok_or_else(|| format!("the gzip trailer runs past the end of the file (byte {at})"))?;
    let want_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let isize = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    Ok(Gzip {
        end: at + 8,
        plain_len,
        isize,
        crc_ok: want_crc == crc,
        head,
    })
}

/// Where the zstd frames at the start of `data` end, walked frame by frame and block by block
/// (RFC 8878, sections 3.1.1 and 3.1.2), without decompressing. Skippable frames count as frames.
/// The walk stops at the first byte that does not start a frame.
pub(crate) fn zstd_end(data: &[u8]) -> Result<usize, String> {
    let mut at = 0usize;
    let mut frames = 0;
    while at + 4 <= data.len() {
        let magic = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
            let n = data
                .get(at + 4..at + 8)
                .ok_or("a skippable zstd frame's size runs past the end")?;
            at += 8 + u32::from_le_bytes([n[0], n[1], n[2], n[3]]) as usize;
            frames += 1;
            continue;
        }
        if magic != 0xFD2F_B528 {
            break;
        }
        at += 4;
        let fhd = *data
            .get(at)
            .ok_or("a zstd frame header runs past the end")?;
        at += 1;
        let fcs_flag = fhd >> 6;
        let single = fhd & 0x20 != 0;
        let checksum = fhd & 0x04 != 0;
        let did = [0usize, 1, 2, 4][usize::from(fhd & 0x03)];
        let fcs = match fcs_flag {
            0 => usize::from(single),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        at += usize::from(!single) + did + fcs;
        loop {
            let h = data
                .get(at..at + 3)
                .ok_or_else(|| format!("a zstd block header runs past the end (byte {at})"))?;
            let h = u32::from(h[0]) | (u32::from(h[1]) << 8) | (u32::from(h[2]) << 16);
            at += 3;
            let last = h & 1 != 0;
            let kind = (h >> 1) & 3;
            let size = (h >> 3) as usize;
            at += match kind {
                1 => 1,
                3 => {
                    return Err(format!(
                        "a zstd block of the reserved type at byte {}",
                        at - 3
                    ));
                }
                _ => size,
            };
            if last {
                break;
            }
        }
        if checksum {
            at += 4;
        }
        if at > data.len() {
            return Err(format!(
                "the zstd frame runs past the end of the file ({at} > {})",
                data.len()
            ));
        }
        frames += 1;
    }
    if frames == 0 {
        return Err("no zstd frame".to_string());
    }
    Ok(at)
}

/// Where a plain newc cpio archive ends: just past its `TRAILER!!!` entry, padded to 4.
pub(crate) fn cpio_end(data: &[u8]) -> Result<usize, String> {
    let mut at = 0usize;
    loop {
        let h = data
            .get(at..at + 110)
            .ok_or_else(|| format!("a cpio header runs past the end of the file (byte {at})"))?;
        if &h[0..6] != b"070701" && &h[0..6] != b"070702" {
            return Err(format!("no cpio header magic at byte {at}"));
        }
        let field = |i: usize| -> Result<usize, String> {
            let s = std::str::from_utf8(&h[6 + 8 * i..14 + 8 * i]).map_err(|e| e.to_string())?;
            usize::from_str_radix(s, 16).map_err(|e| format!("cpio field at byte {at}: {e}"))
        };
        let filesize = field(6)?;
        let namesize = field(11)?;
        let name = data
            .get(at + 110..at + 110 + namesize)
            .ok_or("a cpio name runs past the end of the file")?;
        let trailer = name.strip_suffix(&[0]).unwrap_or(name) == b"TRAILER!!!";
        at = (at + 110 + namesize).next_multiple_of(4);
        at = (at + filesize).next_multiple_of(4);
        if trailer {
            return Ok(at.min(data.len()));
        }
    }
}

/// Whether `data` ends like an xz stream: the footer magic `YZ` as its last two bytes.
pub(crate) fn xz_ends(data: &[u8]) -> bool {
    data.ends_with(b"YZ")
}
