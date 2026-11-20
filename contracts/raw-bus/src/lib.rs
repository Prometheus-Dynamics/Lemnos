//! The `raw-bus/1` contract: raw access to the I2C bus, SPI device, GPIO line, PWM channel and
//! UART a device is wired to. A device has at most one of each, and each method works on the ones it has. Nothing
//! here is a message of its own: a caller asks for a transfer the way it asks for any other method,
//! and each transfer carries its bytes in the call, so one call is one bus transaction.
//!
//! Size limits. A call's arguments and its reply travel as they are:
//!
//! - Over the wire (a client of the service), a call's arguments and its reply may each be as large
//!   as one frame, `lemnos_wire::MAX_FRAME` (1 MiB), less the call's own fixed fields. The reply's
//!   size is the `into` buffer the caller gives.
//! - Between drivers (a `LocalCaller`), the arguments are at most `LOCAL_ARGS` (64) bytes, which
//!   leaves 50 bytes of write for `i2c_transfer` and 52 for `spi_transfer`. A transfer list is
//!   encoded in those same arguments, so a driver-to-driver list holds only a few short ops.
//!
//! Whether a call is allowed is the device's write policy, set in the board file. A call the policy
//! refuses reaches no bus and returns `NOT_PERMITTED`.

#![no_std]

use lemnos_core::{Result, contract};

#[contract("raw-bus", version = 1, kind = "bus")]
pub trait RawBus {
    /// An I2C transaction to the 7-bit `address`: the bytes in `write`, then, if `read_len` is not 0,
    /// a repeated start and `read_len` bytes. The bytes read go into `into`, which must hold
    /// `read_len`, and the number read is returned. An empty `write` with a `read_len` of 0 is
    /// `INVALID_ARGUMENT`.
    #[method(1)]
    fn i2c_transfer(
        &mut self,
        address: u16,
        write: &[u8],
        read_len: u32,
        #[out] into: &mut [u8],
    ) -> Result<usize>;
    /// A full-duplex SPI transfer in one chip-select assertion: `write` is sent while `read_len`
    /// bytes are read into `into`, which must hold `read_len`. The number read is returned. An empty
    /// `write` with a `read_len` of 0 is `INVALID_ARGUMENT`.
    #[method(2)]
    fn spi_transfer(
        &mut self,
        write: &[u8],
        read_len: u32,
        #[out] into: &mut [u8],
    ) -> Result<usize>;
    /// The logical value of the GPIO line.
    #[method(3)]
    fn line_get(&mut self) -> Result<bool>;
    /// Drives the GPIO line to a logical value.
    #[method(4)]
    fn line_set(&mut self, value: bool) -> Result<()>;
    /// Sets the PWM channel's period, duty (both in nanoseconds) and enable.
    #[method(5)]
    fn pwm_set(&mut self, period_ns: u64, duty_ns: u64, enabled: bool) -> Result<()>;
    /// Sends `data` on the UART and returns how many bytes went: fewer than all when the port's
    /// transmit buffer stays full. An empty `data` is `INVALID_ARGUMENT`.
    #[added]
    #[method(6)]
    fn uart_write(&mut self, data: &[u8]) -> Result<u32>;
    /// Up to `max_len` bytes that arrived on the UART, waiting at most `timeout_ms` for the first
    /// (0 does not wait, and more than [`UART_MAX_WAIT_MS`] is `INVALID_ARGUMENT`). The bytes go
    /// into `into`, which must hold `max_len`, and the number is returned: 0 when nothing came.
    /// A `max_len` of 0 is `INVALID_ARGUMENT`.
    #[added]
    #[method(7)]
    fn uart_read(&mut self, max_len: u32, timeout_ms: u32, #[out] into: &mut [u8])
    -> Result<usize>;
    /// One I2C transaction of several segments to the 7-bit `address`: the ops in `ops`, in order,
    /// with a repeated start between them and one stop at the end. Each op is a write of its bytes or
    /// a read of its length (see [`I2cOp`] and [`encode_i2c_ops`]). The bytes read go into `into` one
    /// after another, in op order, and the total is returned. More than [`MAX_I2C_OPS`] ops, an empty
    /// list, an empty op, a malformed list, or reads that do not fit `into` are `INVALID_ARGUMENT`.
    /// Marked `#[added]`: the base hash does not move, and a device without the method is checked per
    /// method.
    #[method(8)]
    #[added]
    fn i2c_transactions(
        &mut self,
        address: u16,
        ops: &[u8],
        #[out] into: &mut [u8],
    ) -> Result<usize>;
    /// A list of SPI transfers, each on its own settings: mode, clock, word size, whether it ends with
    /// a chip-select change, and a delay after it (see [`SpiTransfer`] and [`encode_spi_transfers`]).
    /// The reads go into `into` one after another, in transfer order, and the total is returned. More
    /// than [`MAX_SPI_TRANSFERS`] transfers, an empty list, a malformed list, a mode above 3, a word
    /// size that is neither 0 nor 8 to 32, or reads that do not fit `into` are `INVALID_ARGUMENT`.
    /// Marked `#[added]`, as [`RawBus::i2c_transactions`] is.
    #[method(9)]
    #[added]
    fn spi_transfers(&mut self, transfers: &[u8], #[out] into: &mut [u8]) -> Result<usize>;
    /// Takes the device for the calling client alone. While it holds the claim, every other caller's
    /// call that drives or uses the device is `BUSY`; reading the line (`line_get`) and its edges
    /// (`read_edges`) stay open to everyone. A claim ends with [`RawBus::release`] or when its
    /// caller goes, and then the line and the PWM channel are put in the device's safe state (the
    /// board file's `line_safe` and `pwm_safe`). Claiming again while holding is `OK`. Another
    /// caller's claim is `BUSY`. A call made by Lemnos itself, not by a client, can't claim
    /// (`NOT_PERMITTED`).
    #[method(10)]
    #[added]
    fn claim(&mut self) -> Result<()>;
    /// Ends the caller's claim and puts the device in its safe state. `FAILED_PRECONDITION` when the
    /// caller holds no claim.
    #[method(11)]
    #[added]
    fn release(&mut self) -> Result<()>;
    /// Configures the line, for the caller that holds the claim (`FAILED_PRECONDITION` without
    /// one). `direction` is [`LINE_INPUT`] or [`LINE_OUTPUT`]; `initial` is an output's first
    /// logical value; `bias`, `drive` and `edge` are the `BIAS_*`, `DRIVE_*` and `EDGE_*` values;
    /// `debounce_us` is an input's debounce (0: none). An output needs the device's `writes`
    /// (`NOT_PERMITTED` otherwise). An unknown value is `INVALID_ARGUMENT`.
    #[method(12)]
    #[added]
    #[allow(clippy::too_many_arguments)]
    fn line_configure(
        &mut self,
        direction: u8,
        initial: bool,
        active_low: bool,
        bias: u8,
        drive: u8,
        edge: u8,
        debounce_us: u32,
    ) -> Result<()>;
    /// The line's most recent edges, oldest first, [`EDGE_BYTES`] each (see [`decode_edge`]); the
    /// number of bytes is returned. Reading doesn't consume them: the device keeps the last
    /// [`MAX_EDGES`], and a caller keeps the last sequence number it saw. A gap in the sequence
    /// numbers is edges that were lost (the device's ring or the kernel's queue overflowed). Edges
    /// come only while the line is configured with an `edge` other than [`EDGE_NONE`]. `into`
    /// takes as many whole edges as fit, the newest ones. Subscribe to it to be told of new edges.
    #[method(13, reading = events)]
    #[added]
    fn read_edges(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}

/// `line_configure` directions.
pub const LINE_INPUT: u8 = 0;
pub const LINE_OUTPUT: u8 = 1;
/// `line_configure` bias: as left, pull-up, pull-down, none.
pub const BIAS_AS_IS: u8 = 0;
pub const BIAS_PULL_UP: u8 = 1;
pub const BIAS_PULL_DOWN: u8 = 2;
pub const BIAS_DISABLED: u8 = 3;
/// `line_configure` drive: push-pull, open-drain, open-source.
pub const DRIVE_PUSH_PULL: u8 = 0;
pub const DRIVE_OPEN_DRAIN: u8 = 1;
pub const DRIVE_OPEN_SOURCE: u8 = 2;
/// `line_configure` edges that produce events.
pub const EDGE_NONE: u8 = 0;
pub const EDGE_RISING: u8 = 1;
pub const EDGE_FALLING: u8 = 2;
pub const EDGE_BOTH: u8 = 3;

/// The edges a device keeps for `read_edges`.
pub const MAX_EDGES: usize = 32;
/// Bytes one edge takes in `read_edges`: rising (1), three zero bytes, sequence number (4) and
/// timestamp in monotonic nanoseconds (8), little-endian.
pub const EDGE_BYTES: usize = 16;

/// One edge of `read_edges`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineEdge {
    /// Logical low to high (`false`: high to low).
    pub rising: bool,
    /// The line's sequence number of the edge.
    pub seq: u32,
    /// When it happened, monotonic nanoseconds (the kernel's `CLOCK_MONOTONIC` on Linux).
    pub timestamp_ns: u64,
}

/// Writes one edge into `out`, which must hold [`EDGE_BYTES`].
pub fn encode_edge(edge: &LineEdge, out: &mut [u8; EDGE_BYTES]) {
    *out = [0; EDGE_BYTES];
    out[0] = u8::from(edge.rising);
    out[4..8].copy_from_slice(&edge.seq.to_le_bytes());
    out[8..16].copy_from_slice(&edge.timestamp_ns.to_le_bytes());
}

/// Reads one edge; `None` when `bytes` isn't one.
pub fn decode_edge(bytes: &[u8]) -> Option<LineEdge> {
    let bytes: &[u8; EDGE_BYTES] = bytes.try_into().ok()?;
    if bytes[0] > 1 || bytes[1..4] != [0; 3] {
        return None;
    }
    Some(LineEdge {
        rising: bytes[0] == 1,
        seq: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
        timestamp_ns: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
    })
}

/// The longest `uart_read` waits, in milliseconds: half the host's call deadline.
pub const UART_MAX_WAIT_MS: u32 = 1000;

/// The most ops one `i2c_transactions` list carries.
pub const MAX_I2C_OPS: usize = 8;
/// The most transfers one `spi_transfers` list carries.
pub const MAX_SPI_TRANSFERS: usize = 8;

/// Bytes an I2C op's header takes: its kind (1) and its length (4, little-endian).
const I2C_HEAD: usize = 1 + 4;
/// Bytes an SPI transfer's header takes: mode (1), `cs_change` (1), bits per word (1), speed (4),
/// delay (2), write length (4) and read length (4), all little-endian where they are more than one.
const SPI_HEAD: usize = 1 + 1 + 1 + 4 + 2 + 4 + 4;

/// One op of an I2C list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cOp<'a> {
    /// Writes these bytes.
    Write(&'a [u8]),
    /// Reads this many bytes.
    Read(u32),
}

/// One transfer of an SPI list, on its own settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiTransfer<'a> {
    /// SPI mode, 0 to 3.
    pub mode: u8,
    /// Clock in hertz, 0 for the device's default.
    pub speed_hz: u32,
    /// Bits per word: 0 for 8, or 8 to 32.
    pub bits_per_word: u8,
    /// Deselect the chip after this transfer.
    pub cs_change: bool,
    /// Wait after this transfer, in microseconds.
    pub delay_us: u16,
    /// The bytes clocked out (may be empty).
    pub write: &'a [u8],
    /// How many bytes are clocked in.
    pub read: u32,
}

/// Encodes the ops of an `i2c_transactions` list into `out`, and returns how many bytes it took.
/// `INVALID_ARGUMENT` when `out` is too small or the list would not be accepted.
pub fn encode_i2c_ops(ops: &[I2cOp<'_>], out: &mut [u8]) -> lemnos_core::Result<usize> {
    let mut at = 0;
    for op in ops {
        let (kind, len, payload): (u8, u32, &[u8]) = match *op {
            I2cOp::Write(bytes) => (0, bytes.len() as u32, bytes),
            I2cOp::Read(len) => (1, len, &[]),
        };
        let end = at + I2C_HEAD + payload.len();
        let record = out
            .get_mut(at..end)
            .ok_or(lemnos_core::Status::BUFFER_TOO_SMALL)?;
        record[0] = kind;
        record[1..5].copy_from_slice(&len.to_le_bytes());
        record[I2C_HEAD..].copy_from_slice(payload);
        at = end;
    }
    Ok(at)
}

/// Encodes the transfers of an `spi_transfers` list into `out`, and returns how many bytes it took.
pub fn encode_spi_transfers(
    transfers: &[SpiTransfer<'_>],
    out: &mut [u8],
) -> lemnos_core::Result<usize> {
    let mut at = 0;
    for transfer in transfers {
        let end = at + SPI_HEAD + transfer.write.len();
        let record = out
            .get_mut(at..end)
            .ok_or(lemnos_core::Status::BUFFER_TOO_SMALL)?;
        record[0] = transfer.mode;
        record[1] = u8::from(transfer.cs_change);
        record[2] = transfer.bits_per_word;
        record[3..7].copy_from_slice(&transfer.speed_hz.to_le_bytes());
        record[7..9].copy_from_slice(&transfer.delay_us.to_le_bytes());
        record[9..13].copy_from_slice(&(transfer.write.len() as u32).to_le_bytes());
        record[13..17].copy_from_slice(&transfer.read.to_le_bytes());
        record[SPI_HEAD..].copy_from_slice(transfer.write);
        at = end;
    }
    Ok(at)
}

/// The ops of an encoded I2C list, checked once: `parse` says whether the list is one the driver
/// takes (its count, its shapes, and its reads' total, which must fit the reply buffer). Iterating
/// it then cannot fail.
#[derive(Debug, Clone, Copy)]
pub struct I2cOps<'a> {
    bytes: &'a [u8],
    count: usize,
    read: usize,
}

impl<'a> I2cOps<'a> {
    /// Checks `bytes` as an `i2c_transactions` list whose reads total at most `reply` bytes.
    pub fn parse(bytes: &'a [u8], reply: usize) -> lemnos_core::Result<Self> {
        let mut at = 0;
        let mut count = 0;
        let mut read = 0usize;
        while at < bytes.len() {
            let head = bytes
                .get(at..at + I2C_HEAD)
                .ok_or(lemnos_core::Status::INVALID_ARGUMENT)?;
            let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
            at += I2C_HEAD;
            match head[0] {
                0 => {
                    if len == 0 {
                        return Err(lemnos_core::Status::INVALID_ARGUMENT);
                    }
                    at = at
                        .checked_add(len)
                        .filter(|&end| end <= bytes.len())
                        .ok_or(lemnos_core::Status::INVALID_ARGUMENT)?;
                }
                1 => {
                    if len == 0 {
                        return Err(lemnos_core::Status::INVALID_ARGUMENT);
                    }
                    read = read
                        .checked_add(len)
                        .ok_or(lemnos_core::Status::INVALID_ARGUMENT)?;
                }
                _ => return Err(lemnos_core::Status::INVALID_ARGUMENT),
            }
            count += 1;
        }
        if count == 0 || count > MAX_I2C_OPS || read > reply {
            return Err(lemnos_core::Status::INVALID_ARGUMENT);
        }
        Ok(Self { bytes, count, read })
    }

    /// How many ops the list has.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the list has no ops (never true of a parsed list).
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The reads' total, in bytes: what the call returns.
    pub fn read_total(&self) -> usize {
        self.read
    }

    /// Whether the list needs the device's `writes`: it has no read, or it writes after a read. A
    /// write before the first read is the register pointer of a read, as `i2c_transfer` admits it,
    /// so a list of that shape is a read.
    pub fn needs_writes(&self) -> bool {
        let mut read = false;
        for op in self.iter() {
            match op {
                I2cOp::Read(_) => read = true,
                I2cOp::Write(_) if read => return true,
                I2cOp::Write(_) => {}
            }
        }
        !read
    }

    /// The ops, in order.
    pub fn iter(&self) -> impl Iterator<Item = I2cOp<'a>> + 'a {
        let mut at = 0;
        let bytes = self.bytes;
        core::iter::from_fn(move || {
            let head = bytes.get(at..at + I2C_HEAD)?;
            let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
            at += I2C_HEAD;
            Some(if head[0] == 0 {
                let payload = &bytes[at..at + len as usize];
                at += len as usize;
                I2cOp::Write(payload)
            } else {
                I2cOp::Read(len)
            })
        })
    }
}

/// The transfers of an encoded SPI list, checked once like [`I2cOps`].
#[derive(Debug, Clone, Copy)]
pub struct SpiTransfers<'a> {
    bytes: &'a [u8],
    count: usize,
    read: usize,
}

impl<'a> SpiTransfers<'a> {
    /// Checks `bytes` as an `spi_transfers` list whose reads total at most `reply` bytes.
    pub fn parse(bytes: &'a [u8], reply: usize) -> lemnos_core::Result<Self> {
        let mut at = 0;
        let mut count = 0;
        let mut read = 0usize;
        while at < bytes.len() {
            let head = bytes
                .get(at..at + SPI_HEAD)
                .ok_or(lemnos_core::Status::INVALID_ARGUMENT)?;
            if head[0] > 3 || !(head[2] == 0 || (8..=32).contains(&head[2])) {
                return Err(lemnos_core::Status::INVALID_ARGUMENT);
            }
            let write = spi_write_len(head);
            let transfer_read = spi_read_len(head);
            if write == 0 && transfer_read == 0 {
                return Err(lemnos_core::Status::INVALID_ARGUMENT);
            }
            at = (at + SPI_HEAD)
                .checked_add(write)
                .filter(|&end| end <= bytes.len())
                .ok_or(lemnos_core::Status::INVALID_ARGUMENT)?;
            read = read
                .checked_add(transfer_read as usize)
                .ok_or(lemnos_core::Status::INVALID_ARGUMENT)?;
            count += 1;
        }
        if count == 0 || count > MAX_SPI_TRANSFERS || read > reply {
            return Err(lemnos_core::Status::INVALID_ARGUMENT);
        }
        Ok(Self { bytes, count, read })
    }

    /// How many transfers the list has.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the list has no transfers (never true of a parsed list).
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The reads' total, in bytes: what the call returns.
    pub fn read_total(&self) -> usize {
        self.read
    }

    /// Whether the list needs the device's `writes`: a transfer with no read part is a write.
    pub fn needs_writes(&self) -> bool {
        self.iter().any(|transfer| transfer.read == 0)
    }

    /// The transfers, in order.
    pub fn iter(&self) -> impl Iterator<Item = SpiTransfer<'a>> + 'a {
        let mut at = 0;
        let bytes = self.bytes;
        core::iter::from_fn(move || {
            let head = bytes.get(at..at + SPI_HEAD)?;
            let write = spi_write_len(head);
            let start = at + SPI_HEAD;
            at = start + write;
            Some(SpiTransfer {
                mode: head[0],
                cs_change: head[1] != 0,
                bits_per_word: head[2],
                speed_hz: u32::from_le_bytes([head[3], head[4], head[5], head[6]]),
                delay_us: u16::from_le_bytes([head[7], head[8]]),
                write: &bytes[start..start + write],
                read: spi_read_len(head),
            })
        })
    }
}

/// The write length in an SPI transfer's header.
fn spi_write_len(head: &[u8]) -> usize {
    u32::from_le_bytes([head[9], head[10], head[11], head[12]]) as usize
}

/// The read length in an SPI transfer's header.
fn spi_read_len(head: &[u8]) -> u32 {
    u32::from_le_bytes([head[13], head[14], head[15], head[16]])
}
