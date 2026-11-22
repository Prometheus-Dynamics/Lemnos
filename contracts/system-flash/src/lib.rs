//! The `system.flash/1` contract: writing a whole first-flash image to a board's storage, and
//! checking it. It is for a board that may be blank (no partition table, no data partition, no
//! slot): the image defines the partitions, the one slot and the boot record, and the driver
//! writes it and does not invent any of them.
//!
//! A driver is one target: the board file names it (`path`), and no method takes a device path.
//! The image is streamed: a `write` job takes its bytes in order through `data`, from offset 0,
//! and never holds them. The image is an xz stream (`format` 1) or raw bytes (`format` 0). Its
//! manifest (schema 1, see the crate's documentation) arrives first. A signed image carries its
//! Ed25519 signature record as a second xz stream of the file, and the manifest repeats it; the
//! image's size and SHA-256 are the raw image's, its first stream's decoded bytes.
//!
//! `info` writes JSON (see the crate's documentation). Statuses are `lemnos-abi`'s.
//!
//! A job's result is text in fields: `name=value`, separated by one space, so every reader (a
//! caller, `lemnos-ctl`, the Orion bridge) shows and reads the same form with nothing to guess.
//! [`write_result`] writes it and [`read_result`] reads it; a reader ignores a field it does not
//! know, so a field may be added after these.

#![no_std]

use lemnos_core::{JobId, Result, contract};

/// The image is raw bytes.
pub const FORMAT_RAW: u8 = 0;
/// The image is an xz stream.
pub const FORMAT_XZ: u8 = 1;
/// Most blocks an xz image stream may hold (its index lists them all). A stream with more is refused.
pub const XZ_MAX_BLOCKS: u32 = 8192;
/// The largest dictionary, in MiB, a block of an xz image may declare: the ceiling of `max_dict_mb`.
pub const XZ_MAX_DICT_MIB: u64 = 8;

#[contract("system.flash", version = 1)]
pub trait Flash {
    /// The target, as JSON: its size, whether it holds anything, whether a write may start, and
    /// the last manifest's facts. Returns its length; `BUFFER_TOO_SMALL` when it doesn't fit.
    #[method(1)]
    fn info(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Accepts the manifest the image will be checked against, and its signature record: empty, or
    /// the 108-byte record (`LMNSSIG1`, algorithm 1, the key id, the Ed25519 signature over the
    /// board's signed message). Nothing is written. The signature policy and its outcome are the
    /// crate's documentation. `NOT_SUPPORTED` for another board's manifest (checked first);
    /// `INVALID_ARGUMENT` for a manifest out of shape or a malformed record, in every policy;
    /// `NOT_PERMITTED` for a known key whose signature does not verify, and, when the board's
    /// security switch is on, for an unsigned image or one under an unknown key; `BUSY` while a write runs.
    #[method(2)]
    fn manifest(&mut self, manifest: &[u8], record: &[u8]) -> Result<()>;
    /// Starts writing the image: `size` bytes of stream, in `format`. The job's progress is the
    /// stream bytes taken of `size`; its result is the text [`write_result`] makes:
    /// `raw_bytes=<the raw image's byte count> outcome=<code>` (`0` verified, `1` not checked:
    /// unsigned, `2` not checked: unknown key).
    /// Needs a manifest, `allow_first_flash` and a target that is not the running system.
    #[method(3, job, stall = 900, result = text)]
    fn write(&mut self, size: u64, format: u8) -> Result<JobId>;
    /// The stream's bytes from `offset`, which must be where the last ended. The last of them ends
    /// the stream: the job then checks the image and ends. Only the write's starter may call it.
    #[method(4)]
    fn data(&mut self, offset: u64, bytes: &[u8]) -> Result<()>;
    /// Reads the target back, from its start, for the manifest's image size, and checks it against
    /// the manifest's SHA-256. Only after a write that finished. The job's result is the text
    /// [`write_result`] makes with no outcome: `raw_bytes=<the byte count checked>`.
    #[method(5, job, stall = 600, result = text)]
    fn verify(&mut self) -> Result<JobId>;
}

/// The longest result text: `raw_bytes=` and 20 digits, then ` outcome=` and one digit.
pub const RESULT_MAX: usize = 40;

/// A job's result text, as [`write_result`] makes it.
#[derive(Clone, Copy)]
pub struct ResultText {
    bytes: [u8; RESULT_MAX],
    len: usize,
}

impl ResultText {
    /// The text's bytes (ASCII).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn push(&mut self, bytes: &[u8]) {
        let end = self.len + bytes.len();
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
    }

    fn push_number(&mut self, mut n: u64) {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.push(&digits[at..]);
    }
}

/// A job's result: the raw byte count, and for a write its signature outcome code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobResult {
    pub raw_bytes: u64,
    /// `0` verified, `1` not checked (unsigned), `2` not checked (unknown key). `None` for a verify.
    pub outcome: Option<u8>,
}

/// The result text of a job: `raw_bytes=<raw>`, then ` outcome=<code>` for a write.
#[must_use]
pub fn write_result(raw: u64, outcome: Option<u8>) -> ResultText {
    let mut text = ResultText {
        bytes: [0; RESULT_MAX],
        len: 0,
    };
    text.push(b"raw_bytes=");
    text.push_number(raw);
    if let Some(code) = outcome {
        text.push(b" outcome=");
        text.push_number(u64::from(code));
    }
    text
}

/// Reads a job's result text. `None` when it has no `raw_bytes` field, a field repeats, or a known
/// field's value is out of shape (an outcome other than `0`, `1` or `2`). Unknown fields are skipped.
#[must_use]
pub fn read_result(text: &[u8]) -> Option<JobResult> {
    let mut raw_bytes = None;
    let mut outcome = None;
    for field in text.split(|&b| b == b' ') {
        let at = field.iter().position(|&b| b == b'=')?;
        let (name, value) = (&field[..at], &field[at + 1..]);
        match name {
            b"raw_bytes" if raw_bytes.is_none() => raw_bytes = Some(number(value)?),
            b"outcome" if outcome.is_none() => match value {
                [code @ b'0'..=b'2'] => outcome = Some(code - b'0'),
                _ => return None,
            },
            b"raw_bytes" | b"outcome" => return None,
            _ => {}
        }
    }
    Some(JobResult {
        raw_bytes: raw_bytes?,
        outcome,
    })
}

/// A decimal `u64`: digits only, no sign, no leading zero but for `0` itself.
fn number(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
        return None;
    }
    let mut n: u64 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_is_its_fields_for_every_count_and_outcome() {
        assert_eq!(
            write_result(16384, Some(1)).as_bytes(),
            b"raw_bytes=16384 outcome=1"
        );
        assert_eq!(
            write_result(0, Some(0)).as_bytes(),
            b"raw_bytes=0 outcome=0"
        );
        assert_eq!(write_result(7, None).as_bytes(), b"raw_bytes=7");
        let widest = write_result(u64::MAX, Some(2));
        assert_eq!(
            widest.as_bytes(),
            b"raw_bytes=18446744073709551615 outcome=2"
        );
        assert_eq!(widest.as_bytes().len(), RESULT_MAX);
    }

    /// A count whose eight little-endian bytes happen to be UTF-8 text ("AAAAAAAA") reads as its
    /// number: the form never depends on the bytes.
    #[test]
    fn a_count_whose_bytes_look_like_text_is_still_its_number() {
        let count = u64::from_le_bytes(*b"AAAAAAAA");
        let text = write_result(count, None);
        assert_eq!(text.as_bytes(), b"raw_bytes=4702111234474983745");
        assert_eq!(
            read_result(text.as_bytes()),
            Some(JobResult {
                raw_bytes: count,
                outcome: None
            })
        );
    }

    #[test]
    fn what_is_written_reads_back_and_unknown_fields_are_skipped() {
        for (raw, outcome) in [(0, None), (5, Some(0)), (u64::MAX, Some(2))] {
            let text = write_result(raw, outcome);
            assert_eq!(
                read_result(text.as_bytes()),
                Some(JobResult {
                    raw_bytes: raw,
                    outcome
                })
            );
        }
        assert_eq!(
            read_result(b"raw_bytes=9 later=x outcome=1"),
            Some(JobResult {
                raw_bytes: 9,
                outcome: Some(1)
            })
        );
    }

    #[test]
    fn a_result_out_of_shape_is_refused() {
        for bad in [
            &b""[..],
            b"outcome=1",
            b"raw_bytes=",
            b"raw_bytes=01",
            b"raw_bytes=-1",
            b"raw_bytes=18446744073709551616",
            b"raw_bytes=1 outcome=3",
            b"raw_bytes=1 outcome=none",
            b"raw_bytes=1 raw_bytes=2",
            b"raw_bytes=1  outcome=1",
            &[0x41, 0x41, 0x41, 0x41, 0x41, 0x41, 0x41, 0x41],
        ] {
            assert_eq!(read_result(bad), None, "{:?}", core::str::from_utf8(bad));
        }
    }
}
