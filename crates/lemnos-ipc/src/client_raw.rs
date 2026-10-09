//! Raw bus and line access through `lemnosd`, and control restore.
//!
//! Claims (lines, PWM channels, I2C and SPI locks) belong to the
//! connection: they end when it closes, crashed or not, and the service puts
//! lines in their safe state and disables PWM channels. A client that set
//! [`ClientOptions::keep_intents`](super::ClientOptions::keep_intents) keeps
//! them until it releases them. Claims do not survive a `lemnosd` restart:
//! after a `Connected { reconnects > 0 }` event, claim again.

use super::{ClientError, Connection, DeviceClient, reply};
use crate::wire::{I2cOp, LineTarget, Message, PwmTarget, RawRequest, Refusal, Request, SpiXfer};
use lemnos_hal::raw::{LineConfig, PwmConfig, SafeState, SpiConfig};

fn claimed(id: u32) -> impl FnMut(&Message) -> Option<Result<u32, Refusal>> {
    move |m| match m {
        Message::Claimed { id: got, result } if *got == id => Some(*result),
        _ => None,
    }
}

fn data(id: u32) -> impl FnMut(&Message) -> Option<Result<Vec<u8>, Refusal>> {
    move |m| match m {
        Message::Data { id: got, result } if *got == id => Some(result.clone()),
        _ => None,
    }
}

impl Connection {
    fn raw_reply(&mut self, build: impl FnOnce(u32) -> RawRequest) -> Result<f64, ClientError> {
        let id = self.next_id();
        self.request(&Request::Raw(build(id)), reply(id))?
            .map_err(ClientError::Refused)
    }

    fn raw_claim(&mut self, build: impl FnOnce(u32) -> RawRequest) -> Result<u32, ClientError> {
        let id = self.next_id();
        self.request(&Request::Raw(build(id)), claimed(id))?
            .map_err(ClientError::Refused)
    }

    fn raw_data(&mut self, build: impl FnOnce(u32) -> RawRequest) -> Result<Vec<u8>, ClientError> {
        let id = self.next_id();
        self.request(&Request::Raw(build(id)), data(id))?
            .map_err(ClientError::Refused)
    }
}

/// A claimed GPIO line. Its edges arrive as
/// [`Event::Edge`](crate::Event::Edge) with this handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Line {
    handle: u32,
}

impl Line {
    pub fn handle(self) -> u32 {
        self.handle
    }

    /// The logical value.
    pub fn get(self, client: &mut DeviceClient) -> Result<bool, ClientError> {
        let handle = self.handle;
        client
            .conn
            .raw_reply(|id| RawRequest::LineGet { id, handle })
            .map(|v| v != 0.0)
    }

    /// Sets the logical value of an output.
    pub fn set(self, client: &mut DeviceClient, value: bool) -> Result<(), ClientError> {
        let handle = self.handle;
        client
            .conn
            .raw_reply(|id| RawRequest::LineSet { id, handle, value })
            .map(|_| ())
    }

    /// Changes direction, bias, drive, edges or debounce.
    pub fn configure(
        self,
        client: &mut DeviceClient,
        config: LineConfig,
    ) -> Result<(), ClientError> {
        let handle = self.handle;
        client
            .conn
            .raw_reply(|id| RawRequest::LineConfigure { id, handle, config })
            .map(|_| ())
    }

    /// Ends the claim: the line goes to its safe state.
    pub fn release(self, client: &mut DeviceClient) -> Result<(), ClientError> {
        client.unclaim(self.handle)
    }
}

/// A claimed PWM channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pwm {
    handle: u32,
}

impl Pwm {
    pub fn handle(self) -> u32 {
        self.handle
    }

    /// Sets period, duty, polarity and enable.
    pub fn configure(
        self,
        client: &mut DeviceClient,
        config: PwmConfig,
    ) -> Result<(), ClientError> {
        let handle = self.handle;
        client
            .conn
            .raw_reply(|id| RawRequest::PwmConfigure { id, handle, config })
            .map(|_| ())
    }

    /// Ends the claim: the channel is disabled.
    pub fn release(self, client: &mut DeviceClient) -> Result<(), ClientError> {
        client.unclaim(self.handle)
    }
}

/// An I2C target: a bus (`"1"`, `"i2c-1"` or a board selector such as
/// `"i2c:compatible=i2c-gpio"`) and a 7-bit address. No claim is needed;
/// each call is one atomic transaction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct I2cDevice {
    pub bus: String,
    pub address: u16,
}

impl I2cDevice {
    /// Runs `ops` as one transaction (repeated starts, no stop in between);
    /// returns the bytes read, in order.
    pub fn transfer(
        &self,
        client: &mut DeviceClient,
        ops: Vec<I2cOp>,
    ) -> Result<Vec<u8>, ClientError> {
        let (bus, address) = (self.bus.clone(), self.address);
        client.conn.raw_data(|id| RawRequest::I2cTransfer {
            id,
            bus,
            address,
            ops,
        })
    }

    pub fn write(&self, client: &mut DeviceClient, bytes: &[u8]) -> Result<(), ClientError> {
        self.transfer(client, vec![I2cOp::Write(bytes.to_vec())])
            .map(|_| ())
    }

    pub fn read(&self, client: &mut DeviceClient, len: u16) -> Result<Vec<u8>, ClientError> {
        self.transfer(client, vec![I2cOp::Read(len)])
    }

    /// Writes `bytes`, then reads `len` with a repeated start.
    pub fn write_read(
        &self,
        client: &mut DeviceClient,
        bytes: &[u8],
        len: u16,
    ) -> Result<Vec<u8>, ClientError> {
        self.transfer(client, vec![I2cOp::Write(bytes.to_vec()), I2cOp::Read(len)])
    }

    /// SMBus-style: `len` registers from 8-bit register `register`.
    pub fn read_regs(
        &self,
        client: &mut DeviceClient,
        register: u8,
        len: u16,
    ) -> Result<Vec<u8>, ClientError> {
        self.write_read(client, &[register], len)
    }

    /// SMBus-style: one 8-bit register.
    pub fn read_reg8(&self, client: &mut DeviceClient, register: u8) -> Result<u8, ClientError> {
        self.read_regs(client, register, 1)
            .map(|v| v.first().copied().unwrap_or(0))
    }

    /// SMBus-style: writes `values` from 8-bit register `register`.
    pub fn write_regs(
        &self,
        client: &mut DeviceClient,
        register: u8,
        values: &[u8],
    ) -> Result<(), ClientError> {
        let mut bytes = Vec::with_capacity(values.len() + 1);
        bytes.push(register);
        bytes.extend_from_slice(values);
        self.write(client, &bytes)
    }

    pub fn write_reg8(
        &self,
        client: &mut DeviceClient,
        register: u8,
        value: u8,
    ) -> Result<(), ClientError> {
        self.write_regs(client, register, &[value])
    }

    /// Keeps other clients off this address until [`unlock`](Self::unlock)
    /// or disconnect (for multi-transaction sequences).
    pub fn lock(&self, client: &mut DeviceClient) -> Result<(), ClientError> {
        self.set_lock(client, true)
    }

    pub fn unlock(&self, client: &mut DeviceClient) -> Result<(), ClientError> {
        self.set_lock(client, false)
    }

    fn set_lock(&self, client: &mut DeviceClient, lock: bool) -> Result<(), ClientError> {
        let (bus, address) = (self.bus.clone(), self.address);
        client
            .conn
            .raw_reply(|id| RawRequest::I2cLock {
                id,
                bus,
                address,
                lock,
            })
            .map(|_| ())
    }
}

/// An SPI device: a bus and chip select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpiDevice {
    pub bus: u32,
    pub chip_select: u16,
}

impl SpiDevice {
    /// Runs `transfers` as one transaction (chip select held unless a
    /// transfer asks otherwise); returns every transfer's received bytes,
    /// concatenated in order. One SPI mode per transaction.
    pub fn transfer(
        &self,
        client: &mut DeviceClient,
        transfers: Vec<SpiXfer>,
    ) -> Result<Vec<u8>, ClientError> {
        let (bus, chip_select) = (self.bus, self.chip_select);
        client.conn.raw_data(|id| RawRequest::SpiTransfer {
            id,
            bus,
            chip_select,
            transfers,
        })
    }

    /// One full-duplex transfer of `tx` with `config`; returns as many bytes
    /// as were sent.
    pub fn xfer(
        &self,
        client: &mut DeviceClient,
        tx: &[u8],
        config: SpiConfig,
    ) -> Result<Vec<u8>, ClientError> {
        let mut transfer = SpiXfer::new(tx, u16::try_from(tx.len()).unwrap_or(u16::MAX));
        transfer.config = config;
        self.transfer(client, vec![transfer])
    }

    pub fn lock(&self, client: &mut DeviceClient) -> Result<(), ClientError> {
        self.set_lock(client, true)
    }

    pub fn unlock(&self, client: &mut DeviceClient) -> Result<(), ClientError> {
        self.set_lock(client, false)
    }

    fn set_lock(&self, client: &mut DeviceClient, lock: bool) -> Result<(), ClientError> {
        let (bus, chip_select) = (self.bus, self.chip_select);
        client
            .conn
            .raw_reply(|id| RawRequest::SpiLock {
                id,
                bus,
                chip_select,
                lock,
            })
            .map(|_| ())
    }
}

impl DeviceClient {
    /// Claims a GPIO line for this connection, configured as `config`.
    pub fn claim_line(
        &mut self,
        line: LineTarget,
        config: LineConfig,
    ) -> Result<Line, ClientError> {
        self.claim_line_with(line, config, None)
    }

    /// [`claim_line`](Self::claim_line) with the state the line goes to
    /// when the claim ends (instead of the board's, else high impedance).
    pub fn claim_line_with(
        &mut self,
        line: LineTarget,
        config: LineConfig,
        on_release: Option<SafeState>,
    ) -> Result<Line, ClientError> {
        self.conn
            .raw_claim(|id| RawRequest::LineClaim {
                id,
                line,
                config,
                on_release,
            })
            .map(|handle| Line { handle })
    }

    /// Claims a PWM channel for this connection.
    pub fn claim_pwm(&mut self, pwm: PwmTarget) -> Result<Pwm, ClientError> {
        self.conn
            .raw_claim(|id| RawRequest::PwmClaim { id, pwm })
            .map(|handle| Pwm { handle })
    }

    /// An I2C target on `bus` (no request is sent).
    pub fn i2c(&self, bus: impl Into<String>, address: u16) -> I2cDevice {
        I2cDevice {
            bus: bus.into(),
            address,
        }
    }

    /// An SPI device (no request is sent).
    pub fn spi(&self, bus: u32, chip_select: u16) -> SpiDevice {
        SpiDevice { bus, chip_select }
    }

    /// Ends a line or PWM claim by handle.
    pub fn unclaim(&mut self, handle: u32) -> Result<(), ClientError> {
        self.conn
            .raw_reply(|id| RawRequest::Unclaim { id, handle })
            .map(|_| ())
    }

    /// Undoes this client's writes to `device`'s `control` (`None`: every
    /// control): a fan goes back to the kernel, other controls to their
    /// value from before this client's first write. Automatic on disconnect
    /// unless the client keeps its intents.
    pub fn restore(&mut self, device: &str, control: Option<&str>) -> Result<(), ClientError> {
        let id = self.conn.next_id();
        let request = Request::Restore {
            id,
            device: device.into(),
            control: control.unwrap_or_default().into(),
        };
        self.conn
            .request(&request, reply(id))?
            .map(|_| ())
            .map_err(ClientError::Refused)
    }
}
