//! The burner's write path: erase, write, verify by SHA-256, reboot. This is the only module that
//! changes a medium. It works on a [`Burner`] that has probed and read its info.
//!
//! Follows k230_flash 33a75f0 `burner_uboot.cpp` ("U:n" below is a line of that file).

use std::io::Read;

use sha2::{Digest, Sha256};

use crate::burner::Burner;
use crate::hex;
use crate::packet::{REBOOT_MARK, cmd, le64, packet, words};
use crate::transport::{ROM_TIMEOUT_MS, UsbError, VERIFY_TIMEOUT_MS};

impl Burner<'_> {
    /// U:`kburn_erase` (C:570-631): bounds and write protection are checked first, and a reply that
    /// times out is waited for again, 3 to 120 times.
    pub fn erase(&mut self, offset: u64, size: u64) -> Result<(), UsbError> {
        let mut retries = 3;
        if let Some(info) = self.info {
            if offset > info.capacity || size > info.capacity - offset {
                return Err(UsbError::Other("the erase runs past the medium".into()));
            }
            if info.write_protected {
                return Err(UsbError::Other("the medium is write-protected".into()));
            }
            retries = (size / info.erase_size.max(1) + 2).clamp(3, 120);
        }
        self.send(cmd::ERASE, &words(&[offset, size]))?;
        let mut tried = 0;
        loop {
            match self.reply(cmd::ERASE, "size 0") {
                Err(UsbError::Timeout(_)) if tried < retries => tried += 1,
                other => return other.map(drop),
            }
        }
    }

    /// Writes `size` bytes of `image` at `offset`, into a region of `region` bytes (padded by the
    /// burner; on NAND it skips bad blocks inside it). As the vendor tool does, the size sent is
    /// rounded up to the block size and the tail is zero padding. Needs `probe` and `info` first.
    /// Returns the SHA-256 of the `size` image bytes, which [`Burner::sha256`] checks.
    pub fn write(
        &mut self,
        offset: u64,
        image: &mut dyn Read,
        size: u64,
        region: u64,
        progress: &mut dyn FnMut(u64, u64),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String, UsbError> {
        let (Some(info), true) = (self.info, self.out_chunk != 0) else {
            return Err(UsbError::Other("write before probe and info".into()));
        };
        let refuse = |message: &str| UsbError::Other(message.into());
        let block = info.block_size;
        if size == 0 || block == 0 || info.erase_size == 0 {
            return Err(refuse("nothing to write, or a medium without blocks"));
        }
        let total = size.div_ceil(block) * block;
        let chunk = self.out_chunk / block * block;
        if chunk == 0 {
            return Err(refuse("the write chunk is smaller than a block"));
        }
        // C:`kburn_write_start` (C:633-709).
        if offset > info.capacity || total > info.capacity - offset {
            return Err(refuse("the write runs past the medium"));
        }
        if region != 0 && (total > region || region > info.capacity - offset) {
            return Err(refuse("the write exceeds its region"));
        }
        if info.write_protected {
            return Err(refuse("the medium is write-protected"));
        }
        if !offset.is_multiple_of(info.erase_size) {
            return Err(refuse("the write start isn't aligned to the erase size"));
        }
        // Protocol 1 added the partition flag word.
        let config = if self.loader_version >= 1 {
            words(&[offset, total, region, 0])
        } else {
            words(&[offset, total, region])
        };
        self.command(cmd::WRITE, &config, "size 0 (the start)")?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; chunk as usize];
        let mut sent = 0u64;
        while sent < total {
            if cancelled() {
                return Err(UsbError::Other(
                    "cancelled mid-write; the medium is incomplete".into(),
                ));
            }
            let take = (total - sent).min(chunk) as usize;
            let logical = size.saturating_sub(sent).min(take as u64) as usize;
            image
                .read_exact(&mut buffer[..logical])
                .map_err(|error| UsbError::Other(format!("reading the image: {error}")))?;
            hasher.update(&buffer[..logical]);
            buffer[logical..take].fill(0);
            self.write_chunk(&buffer[..take], self.out_chunk.min(total - sent))?;
            sent += take as u64;
            progress(sent.min(size), size);
        }
        self.reply(cmd::WRITE, "size 0 (the end)")?;
        self.command(cmd::NOP, &[], "size 0")?;
        Ok(hex(&hasher.finalize()))
    }

    /// C:`kburn_write_chunk` (C:711-751): a chunk shorter than the burner expects that fills its last
    /// packet exactly is followed by a zero-length packet; a failed write reads the burner's reply
    /// for the reason.
    fn write_chunk(&mut self, data: &[u8], expected: u64) -> Result<(), UsbError> {
        if let Err(error) = self.io.bulk_out(data) {
            if error == UsbError::Disconnected {
                return Err(error);
            }
            return match self.reply(cmd::WRITE, "an error reply") {
                Err(reason) => Err(reason),
                Ok(_) => Err(error),
            };
        }
        if (data.len() as u64) < expected && data.len().is_multiple_of(self.max_packet) {
            self.io.bulk_out(&[])?;
        }
        Ok(())
    }

    /// The burner's SHA-256 of `size` bytes at `offset` (C:`kburn_verify_sha256`, C:369-432; loader
    /// version 2 and up). The reply is exactly 48 bytes: the digest, the byte count it covered, the
    /// milliseconds it took. Any other length, or a different count, is an error and not a match.
    /// (The transport's timeout is raised to [`VERIFY_TIMEOUT_MS`] for this call and restored after.)
    pub fn sha256(&mut self, offset: u64, size: u64) -> Result<String, UsbError> {
        if self.loader_version < 2 {
            return Err(UsbError::Other(
                "the loader does not support verification".into(),
            ));
        }
        self.io.set_timeout_ms(VERIFY_TIMEOUT_MS);
        let reply = self.command(
            cmd::VERIFY,
            &words(&[offset, size]),
            "size 48: sha256, verified bytes u64, elapsed ms u64",
        );
        self.io.set_timeout_ms(ROM_TIMEOUT_MS);
        let data = reply?;
        if data.len() != 48 {
            return Err(UsbError::Protocol(format!(
                "a {}-byte verify reply, not 48",
                data.len()
            )));
        }
        let verified = le64(&data, 32);
        if verified != size {
            return Err(UsbError::Protocol(format!(
                "the burner verified {verified} bytes, not {size}"
            )));
        }
        Ok(hex(&data[..32]))
    }

    /// C:`kburn_reset_chip` (C:484-501): the command with the mark as a u64, no reply read. A
    /// disconnect after it is the reboot.
    pub fn reboot(&mut self) -> Result<(), UsbError> {
        let mark = packet(cmd::REBOOT, &REBOOT_MARK.to_le_bytes());
        match self.io.bulk_out(&mark) {
            Err(UsbError::Disconnected) => Ok(()),
            other => other,
        }
    }
}
