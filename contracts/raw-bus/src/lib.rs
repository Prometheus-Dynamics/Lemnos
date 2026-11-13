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
//!   leaves 50 bytes of write for `i2c_transfer` and 52 for `spi_transfer`.
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
    #[method(6)]
    fn uart_write(&mut self, data: &[u8]) -> Result<u32>;
    /// Up to `max_len` bytes that arrived on the UART, waiting at most `timeout_ms` for the first
    /// (0 does not wait, and more than [`UART_MAX_WAIT_MS`] is `INVALID_ARGUMENT`). The bytes go
    /// into `into`, which must hold `max_len`, and the number is returned: 0 when nothing came.
    /// A `max_len` of 0 is `INVALID_ARGUMENT`.
    #[method(7)]
    fn uart_read(&mut self, max_len: u32, timeout_ms: u32, #[out] into: &mut [u8])
    -> Result<usize>;
}

/// The longest `uart_read` waits, in milliseconds: half the host's call deadline.
pub const UART_MAX_WAIT_MS: u32 = 1000;
