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

#![no_std]

use lemnos_core::{JobId, Result, contract};

/// The image is raw bytes.
pub const FORMAT_RAW: u8 = 0;
/// The image is an xz stream.
pub const FORMAT_XZ: u8 = 1;

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
    /// `NOT_PERMITTED` for a known key whose signature does not verify, and, when `require_signed`
    /// is set, for an unsigned image or one under an unknown key; `BUSY` while a write runs.
    #[method(2)]
    fn manifest(&mut self, manifest: &[u8], record: &[u8]) -> Result<()>;
    /// Starts writing the image: `size` bytes of stream, in `format`. The job's progress is the
    /// stream bytes taken of `size`; its result is the raw image's byte count, `u64` little-endian,
    /// then one outcome code byte (0 verified, 1 not checked: unsigned, 2 not checked: unknown key).
    /// Needs a manifest, `allow_first_flash` and a target that is not the running system.
    #[method(3, job)]
    fn write(&mut self, size: u64, format: u8) -> Result<JobId>;
    /// The stream's bytes from `offset`, which must be where the last ended. The last of them ends
    /// the stream: the job then checks the image and ends. Only the write's starter may call it.
    #[method(4)]
    fn data(&mut self, offset: u64, bytes: &[u8]) -> Result<()>;
    /// Reads the target back, from its start, for the manifest's image size, and checks it against
    /// the manifest's SHA-256. Only after a write that finished. The job's result is the byte count
    /// checked, `u64` little-endian.
    #[method(5, job)]
    fn verify(&mut self) -> Result<JobId>;
}
