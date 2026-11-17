//! The flasher: writes an image to a medium in an order that is never half-bootable, and checks
//! it before the boot header is written.
//!
//! The layout: the boot header is the first block of the medium ([`HEADER_OFFSET`], 4 KiB) and the
//! payload starts after it ([`PAYLOAD_OFFSET`]). The boot ROM is taken to boot a medium whose
//! header it accepts (the weakest test, [`lemnos_k230_usbboot_stage::format::rom_accepts`]).
//!
//! Write order:
//! 1. Erase the header block and read it back: no valid header may remain.
//! 2. Write the payload in pieces. Each piece is erased, written, and read back equal, up to
//!    [`CHUNK_ATTEMPTS`] times.
//! 3. Read the whole payload back and compare its SHA-256 with the image's. This is before the
//!    header on purpose: a mismatch leaves no header, so the ROM stays in download mode.
//! 4. Write the header last, then read it back.
//!
//! A cut or reset at any point leaves either no valid header (the ROM falls back to download mode,
//! so the board is recoverable over USB) or a valid header over a complete, checked payload. The
//! stage refuses a write that breaks this order too; see `lemnos-k230-usbboot-stage`.
//!
//! Deviation from the brief's order: the brief names the whole-image check after the header. It
//! is here before the header, so the header is the commit point and a failed check leaves nothing
//! booted. The check reads the same bytes either way.

use std::fmt;

use lemnos_k230_usbboot_stage::format::{BOOT_HEADER_LEN, BootHeader, HEADER_BLOCK, rom_accepts};
use sha2::{Digest, Sha256};

use crate::burner::{Burner, Medium};
use crate::hex;
use crate::transport::UsbError;

/// The boot header's offset on the medium.
pub const HEADER_OFFSET: u64 = 0;
/// The payload's offset on the medium: after the header block.
pub const PAYLOAD_OFFSET: u64 = HEADER_BLOCK;
/// Attempts at writing one piece before the flash gives up on it (each reads back).
pub const CHUNK_ATTEMPTS: u32 = 3;
/// The first wait before a retry, and the cap on every later one, in milliseconds.
pub const BACKOFF_FIRST_MS: u32 = 5_000;
pub const BACKOFF_CAP_MS: u32 = 10_000;

/// The waits between retries: the first is [`BACKOFF_FIRST_MS`], each after doubles, capped at
/// [`BACKOFF_CAP_MS`]. Every wait is between 5 and 10 seconds, and none is longer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    next_ms: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            next_ms: BACKOFF_FIRST_MS,
        }
    }

    /// The next wait, in milliseconds.
    pub fn next_ms(&mut self) -> u32 {
        let wait = self.next_ms;
        self.next_ms = (self.next_ms * 2).min(BACKOFF_CAP_MS);
        wait
    }
}

/// Tries `attempt` until it gives a value or `budget_ms` of waiting has run out, sleeping between
/// tries with [`Backoff`]. The budget is the deadline: a retry that cannot be waited for within it
/// is not made.
pub fn retry_until<T>(
    budget_ms: u32,
    mut attempt: impl FnMut() -> Option<T>,
    mut sleep_ms: impl FnMut(u32),
) -> Result<T, FlashError> {
    let mut backoff = Backoff::new();
    let mut waited = 0u32;
    loop {
        if let Some(value) = attempt() {
            return Ok(value);
        }
        if waited >= budget_ms {
            return Err(FlashError::NotInStage);
        }
        let wait = backoff.next_ms().min(budget_ms - waited);
        sleep_ms(wait);
        waited += wait;
    }
}

/// Why a flash failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlashError {
    /// A transfer or command failed.
    Usb(UsbError),
    /// The image or the medium does not fit the plan.
    BadPlan(String),
    /// A piece did not read back equal after every attempt.
    Verify { offset: u64, attempts: u32 },
    /// The payload's SHA-256 on the medium is not the image's. Nothing was booted.
    ImageHash { expected: String, found: String },
    /// A valid header survived the erase of the header block.
    HeaderStillValid,
    /// The header read back differs from the header written.
    HeaderNotReadBack,
    /// The stage did not come back in time after a reset.
    NotInStage,
}

impl fmt::Display for FlashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usb(error) => write!(f, "{error}"),
            Self::BadPlan(reason) => write!(f, "bad plan: {reason}"),
            Self::Verify { offset, attempts } => {
                write!(
                    f,
                    "the piece at {offset:#x} did not read back after {attempts} attempts"
                )
            }
            Self::ImageHash { expected, found } => {
                write!(
                    f,
                    "the payload's SHA-256 {found} is not the image's {expected}"
                )
            }
            Self::HeaderStillValid => write!(f, "a valid boot header survived the erase"),
            Self::HeaderNotReadBack => write!(f, "the boot header did not read back as written"),
            Self::NotInStage => write!(f, "the stage did not come back in time"),
        }
    }
}

impl std::error::Error for FlashError {}

impl From<UsbError> for FlashError {
    fn from(error: UsbError) -> Self {
        Self::Usb(error)
    }
}

/// What a successful flash did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The pieces the payload was written in.
    pub pieces: u32,
    /// Piece writes made, retries included.
    pub piece_writes: u32,
    /// The payload's SHA-256, as the stage reported it.
    pub image_sha256: String,
}

fn round_up(value: u64, unit: u64) -> u64 {
    value.div_ceil(unit) * unit
}

/// Steps 1 and 2: invalidates the header and writes the payload, each piece read back equal. The
/// burner is probed and its info read here. Returns the pieces written.
pub fn payload(
    burner: &mut Burner<'_>,
    medium: Medium,
    image: &[u8],
) -> Result<(u32, u32), FlashError> {
    if image.is_empty() {
        return Err(FlashError::BadPlan("there is no image".into()));
    }
    burner.probe(medium)?;
    let info = burner.info()?;
    let (block, erase) = (info.block_size, info.erase_size);
    if block == 0
        || erase == 0
        || !erase.is_multiple_of(block)
        || !HEADER_BLOCK.is_multiple_of(erase)
    {
        return Err(FlashError::BadPlan(
            "the medium's blocks do not fit the boot header block".into(),
        ));
    }
    let size = image.len() as u64;
    if PAYLOAD_OFFSET + size > info.capacity {
        return Err(FlashError::BadPlan(
            "the image does not fit the medium after the header".into(),
        ));
    }
    let piece = burner.out_chunk() / erase * erase;
    if piece == 0 {
        return Err(FlashError::BadPlan(
            "the burner's chunk is smaller than an erase block".into(),
        ));
    }

    // 1. Invalidate: erase the header block; no valid header may remain.
    burner.erase(HEADER_OFFSET, HEADER_BLOCK)?;
    let erased = burner.read(HEADER_OFFSET, BOOT_HEADER_LEN as u64)?;
    if rom_accepts(&erased) {
        return Err(FlashError::HeaderStillValid);
    }

    // 2. The payload, piece by piece, each read back equal.
    let mut at = 0u64;
    let mut pieces = 0u32;
    let mut piece_writes = 0u32;
    while at < size {
        let take = (size - at).min(piece);
        let bytes = &image[at as usize..(at + take) as usize];
        let offset = PAYLOAD_OFFSET + at;
        let mut verified = false;
        for _ in 0..CHUNK_ATTEMPTS {
            piece_writes += 1;
            burner.erase(offset, round_up(take, erase))?;
            let mut source = bytes;
            match burner.write(offset, &mut source, take, 0, &mut |_, _| {}, &|| false) {
                Ok(_) => {}
                // The stage refused the piece (its read-back differed): try it again.
                Err(UsbError::Protocol(_)) => continue,
                Err(error) => return Err(error.into()),
            }
            if burner.read(offset, take)? == bytes {
                verified = true;
                break;
            }
        }
        if !verified {
            return Err(FlashError::Verify {
                offset,
                attempts: CHUNK_ATTEMPTS,
            });
        }
        at += take;
        pieces += 1;
    }
    Ok((pieces, piece_writes))
}

/// Steps 3 and 4: the whole payload hashed on the medium against the image, then the header, last,
/// read back. Only a payload that matches gets its header.
pub fn commit(burner: &mut Burner<'_>, image: &[u8]) -> Result<String, FlashError> {
    let size = image.len() as u64;
    // 3. The whole payload, hashed on the medium, against the image.
    let expected = hex(&Sha256::digest(image));
    let found = burner.sha256(PAYLOAD_OFFSET, size)?;
    if found != expected {
        return Err(FlashError::ImageHash { expected, found });
    }

    // 4. The header, last. It names the payload just checked.
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&Sha256::digest(image));
    let header = BootHeader {
        payload_offset: PAYLOAD_OFFSET,
        payload_size: size,
        digest,
    }
    .encode();
    let mut block_bytes = vec![0u8; HEADER_BLOCK as usize];
    block_bytes[..BOOT_HEADER_LEN].copy_from_slice(&header);
    let mut source: &[u8] = &block_bytes;
    burner.write(
        HEADER_OFFSET,
        &mut source,
        HEADER_BLOCK,
        0,
        &mut |_, _| {},
        &|| false,
    )?;
    if burner.read(HEADER_OFFSET, BOOT_HEADER_LEN as u64)? != header {
        return Err(FlashError::HeaderNotReadBack);
    }
    Ok(found)
}

/// Writes `image` to `medium` through `burner`, in the order above: [`payload`], then [`commit`].
/// The burner is probed and its info read here. The caller reboots afterwards.
pub fn flash(burner: &mut Burner<'_>, medium: Medium, image: &[u8]) -> Result<Report, FlashError> {
    let (pieces, piece_writes) = payload(burner, medium, image)?;
    let image_sha256 = commit(burner, image)?;
    Ok(Report {
        pieces,
        piece_writes,
        image_sha256,
    })
}
