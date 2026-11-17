//! The read-only queries of a running burner: its loader version, the probe (the medium's
//! chunk sizes), the medium's info, and reads. Nothing here changes the medium: the write path is
//! in [`crate::write`].
//!
//! Follows k230_flash 33a75f0 `burner_uboot.cpp`; "C:n" below is a line of that file.

use crate::packet::{DATA, PACKET, REPLY_FLAG, RESULT_MESSAGE, RESULT_OK, STALE_REPLIES};
use crate::packet::{cmd, le16, le64, packet, words};
use crate::transport::{UsbError, UsbIo};

/// The storage a burner can drive: `kburn.h` `KBurnMediumType` (INVALID 0 is not a choice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Medium {
    Emmc = 1,
    Sd = 2,
    SpiNand = 3,
    SpiNor = 4,
    Otp = 5,
}

/// The medium as the burner describes it (`DEV_GET_INFO`: `kburn_medium_info`, kburn_k230.h:46-56).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediumInfo {
    pub capacity: u64,
    pub block_size: u64,
    pub erase_size: u64,
    pub write_protected: bool,
    pub valid: bool,
}

/// One burner session over an open device. The write path in [`crate::write`] works on the same
/// session, so its fields are crate-visible.
pub struct Burner<'a> {
    pub(crate) io: &'a mut dyn UsbIo,
    /// Bytes per bulk write, from the probe.
    pub(crate) out_chunk: u64,
    /// Bytes per bulk read, from the probe.
    pub(crate) in_chunk: u64,
    pub(crate) max_packet: usize,
    /// 0 until read; C:kburn_probe_loader_version.
    pub(crate) loader_version: u32,
    pub(crate) synced: bool,
    pub(crate) info: Option<MediumInfo>,
}

impl std::fmt::Debug for Burner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Burner")
            .field("loader_version", &self.loader_version)
            .field("out_chunk", &self.out_chunk)
            .field("in_chunk", &self.in_chunk)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl<'a> Burner<'a> {
    pub fn new(io: &'a mut dyn UsbIo) -> Self {
        let max_packet = io.max_packet_out();
        Self {
            io,
            out_chunk: 0,
            in_chunk: 0,
            max_packet,
            loader_version: 0,
            synced: false,
            info: None,
        }
    }

    /// The loader's protocol version (0 before the first probe).
    pub fn loader_version(&self) -> u32 {
        self.loader_version
    }

    /// The medium's info, once [`Burner::info`] has read it.
    pub fn medium_info(&self) -> Option<MediumInfo> {
        self.info
    }

    /// The bytes per bulk write the probe gave.
    pub fn out_chunk(&self) -> u64 {
        self.out_chunk
    }

    /// Sends one command packet. Crate-visible for the write path.
    pub(crate) fn send(&mut self, command: u16, data: &[u8]) -> Result<(), UsbError> {
        let packet = packet(command, data);
        self.io.bulk_out(&packet)
    }

    /// Sends a command and returns its reply's data.
    pub(crate) fn command(
        &mut self,
        command: u16,
        data: &[u8],
        expected: &str,
    ) -> Result<Vec<u8>, UsbError> {
        self.send(command, data)?;
        self.reply(command, expected)
    }

    /// C:`kburn_read_response` (C:295-329) and `kburn_parse_resp` (C:241-293): replies are whole
    /// 60-byte packets; one whose command isn't the awaited one, whose size is over 54, or (for NOP)
    /// whose result isn't OK is a stale reply and is skipped, at most 8 times.
    pub(crate) fn reply(&mut self, command: u16, expected: &str) -> Result<Vec<u8>, UsbError> {
        let awaited = command | REPLY_FLAG;
        for _ in 0..=STALE_REPLIES {
            let reply = self.io.bulk_in(PACKET)?;
            if reply.len() != PACKET {
                return Err(UsbError::Protocol(format!(
                    "a {}-byte reply, not {PACKET} (expected {expected})",
                    reply.len()
                )));
            }
            let (got, result, size) = (
                le16(&reply, 0),
                le16(&reply, 2),
                usize::from(le16(&reply, 4)),
            );
            if size > DATA || got != awaited || (command == cmd::NOP && result != RESULT_OK) {
                continue;
            }
            let data = reply[6..6 + size].to_vec();
            return match result {
                RESULT_OK => Ok(data),
                RESULT_MESSAGE => Err(UsbError::Protocol(format!(
                    "the burner refused command {command:#04x} (result {result:#04x}): {}",
                    String::from_utf8_lossy(&data)
                        .split('\0')
                        .next()
                        .unwrap_or_default()
                ))),
                result => Err(UsbError::Protocol(format!(
                    "the burner failed command {command:#04x} (result {result})"
                ))),
            };
        }
        Err(UsbError::Protocol(format!(
            "no reply to command {command:#04x} after {STALE_REPLIES} stale ones"
        )))
    }

    /// C:872-897 (the constructor): the loader version (vendor IN request 0, wValue 1, 4 bytes; any
    /// failure reads as 0), then a NOP to clear the burner's error state.
    fn sync(&mut self) -> Result<(), UsbError> {
        if self.synced {
            return Ok(());
        }
        self.loader_version = match self.io.control_in(0, 1, 0, 4) {
            Ok(bytes) if bytes.len() == 4 => {
                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            }
            Err(UsbError::Disconnected) => return Err(UsbError::Disconnected),
            _ => 0,
        };
        self.command(cmd::NOP, &[], "size 0")?;
        self.synced = true;
        Ok(())
    }

    /// Picks the medium (C:`kburn_probe`, C:503-543); the burner says how much to send per bulk
    /// write and read. Changes no data.
    pub fn probe(&mut self, medium: Medium) -> Result<(), UsbError> {
        self.sync()?;
        let data = self.command(
            cmd::PROBE,
            &[medium as u8, 0xff],
            "size 16: out chunk u64 (1..=INT_MAX), in chunk u64 (1..=65535)",
        )?;
        if data.len() != 16 {
            return Err(UsbError::Protocol(format!(
                "a {}-byte probe reply, not 16",
                data.len()
            )));
        }
        let (out, input) = (le64(&data, 0), le64(&data, 8));
        if out == 0 || out > i32::MAX as u64 || input == 0 || input > u64::from(u16::MAX) {
            return Err(UsbError::Protocol(format!(
                "chunk sizes {out} out and {input} in"
            )));
        }
        self.out_chunk = out;
        self.in_chunk = input;
        Ok(())
    }

    /// C:`kburn_get_capacity` (C:545-568): the reply is exactly the 32-byte struct.
    pub fn info(&mut self) -> Result<MediumInfo, UsbError> {
        let data = self.command(
            cmd::GET_INFO,
            &[],
            "size 32: capacity, block, erase size, then timeout_ms:32 wp:8 type:7 valid:1",
        )?;
        if data.len() != 32 {
            return Err(UsbError::Protocol(format!(
                "a {}-byte info reply, not 32",
                data.len()
            )));
        }
        let bits = le64(&data, 24);
        let info = MediumInfo {
            capacity: le64(&data, 0),
            block_size: le64(&data, 8),
            erase_size: le64(&data, 16),
            write_protected: (bits >> 32) & 0xff != 0,
            valid: (bits >> 47) & 1 == 1,
        };
        self.info = Some(info);
        Ok(info)
    }

    /// C:`K230UBOOTBurner::read` (C:1076-1150): `size` bytes at `offset`, read as whole blocks in
    /// chunks of the probe's input size. Needs `probe` and `info` first.
    pub fn read(&mut self, offset: u64, size: u64) -> Result<Vec<u8>, UsbError> {
        let (Some(info), true) = (self.info, self.in_chunk != 0) else {
            return Err(UsbError::Other("read before probe and info".into()));
        };
        let block = info.block_size;
        if size == 0 || block == 0 || offset > info.capacity {
            return Err(UsbError::Other(
                "nothing to read, or a medium without blocks".into(),
            ));
        }
        let start = offset - offset % block;
        let prefix = offset - start;
        let span = (prefix + size).div_ceil(block) * block;
        if span > info.capacity - start {
            return Err(UsbError::Other("the read runs past the medium".into()));
        }
        self.command(cmd::READ, &words(&[start, span]), "size 0 (the start)")?;
        let mut data = Vec::with_capacity(span as usize);
        while (data.len() as u64) < span {
            let take = (span - data.len() as u64).min(self.in_chunk) as usize;
            data.extend_from_slice(&self.read_chunk(take)?);
        }
        self.reply(cmd::READ_CHUNK, "size 0 (the end)")?;
        Ok(data[prefix as usize..(prefix + size) as usize].to_vec())
    }

    /// C:`kburn_read_chunk` (C:804-857): one whole packet of 6 + `size` bytes, retried on a timeout
    /// up to 3 times.
    fn read_chunk(&mut self, size: usize) -> Result<Vec<u8>, UsbError> {
        let mut tried = 0;
        let reply = loop {
            match self.io.bulk_in(6 + size) {
                Err(UsbError::Timeout(_)) if tried < 3 => tried += 1,
                other => break other?,
            }
        };
        if reply.len() != 6 + size
            || le16(&reply, 0) != cmd::READ_CHUNK | REPLY_FLAG
            || le16(&reply, 2) != RESULT_OK
            || usize::from(le16(&reply, 4)) != size
        {
            return Err(UsbError::Protocol(format!(
                "a bad read chunk: {} bytes, expected {} with cmd 0x8024 and result 1",
                reply.len(),
                6 + size
            )));
        }
        Ok(reply[6..].to_vec())
    }
}
