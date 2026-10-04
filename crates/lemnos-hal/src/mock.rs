//! In-memory doubles for tests (feature `mock`, needs `alloc`): an I2C bus
//! with register-file targets, an SPI device, pins, a delay, a regulator and a
//! clock. Each implements the blocking and the async embedded-hal traits and
//! uses [`ErrorKind`] as its error, so failures can be injected by kind.

extern crate alloc;

use crate::ErrorKind;
use crate::power::{ClockOutput, Regulator};
use crate::register::AddressWidth;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::future::Future;
use core::task::{Context, Poll, Waker};
use embedded_hal::digital::{ErrorType as PinErrorType, InputPin, OutputPin, StatefulOutputPin};
use embedded_hal::i2c::{ErrorType as I2cErrorType, I2c, Operation as I2cOperation};
use embedded_hal::spi::{ErrorType as SpiErrorType, Operation as SpiOperation, SpiDevice};

/// Runs a future that completes without waiting on real events (the mocks
/// here never return `Pending`) to completion.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = core::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
    }
}

/// One recorded bus operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockOp {
    /// Bytes written.
    Write(Vec<u8>),
    /// A read of this many bytes.
    Read(usize),
    /// A full-duplex transfer writing these bytes.
    Transfer(Vec<u8>),
    /// A delay inside an SPI transaction.
    DelayNs(u32),
}

/// One recorded I2C transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I2cTransfer {
    /// The 7-bit target address.
    pub address: u8,
    /// The operations, as given.
    pub ops: Vec<MockOp>,
}

/// A target on a [`MockI2c`] bus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockI2cTarget {
    /// Register contents (unset registers read as 0).
    pub registers: BTreeMap<u16, u8>,
    /// Register address width; `None` for a target without registers (its
    /// writes are only recorded in `raw_writes`).
    pub width: Option<AddressWidth>,
    /// Every contiguous write run, as the target saw it.
    pub raw_writes: Vec<Vec<u8>>,
    pointer: u16,
}

/// An I2C bus with register-file targets that auto-increment their register
/// pointer, as most sensors do. Unknown addresses do not acknowledge.
#[derive(Debug, Clone, Default)]
pub struct MockI2c {
    targets: BTreeMap<u8, MockI2cTarget>,
    log: Vec<I2cTransfer>,
    fail: VecDeque<ErrorKind>,
}

impl MockI2c {
    /// An empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a register-file target with `width` register addresses.
    pub fn with_target(mut self, address: u8, width: AddressWidth) -> Self {
        self.targets.insert(
            address,
            MockI2cTarget {
                width: Some(width),
                ..MockI2cTarget::default()
            },
        );
        self
    }

    /// Adds a target without registers (e.g. a DW9714 VCM).
    pub fn with_raw_target(mut self, address: u8) -> Self {
        self.targets.insert(address, MockI2cTarget::default());
        self
    }

    /// Presets consecutive registers of a target.
    pub fn with_registers(mut self, address: u8, register: u16, bytes: &[u8]) -> Self {
        let target = self.targets.entry(address).or_default();
        for (i, b) in bytes.iter().enumerate() {
            target.registers.insert(register.wrapping_add(i as u16), *b);
        }
        self
    }

    /// The target at `address`.
    pub fn target(&self, address: u8) -> Option<&MockI2cTarget> {
        self.targets.get(&address)
    }

    /// A register's value (0 if unset or no such target).
    pub fn register(&self, address: u8, register: u16) -> u8 {
        self.targets
            .get(&address)
            .and_then(|t| t.registers.get(&register).copied())
            .unwrap_or(0)
    }

    /// Every transaction so far.
    pub fn transfers(&self) -> &[I2cTransfer] {
        &self.log
    }

    /// Forgets recorded transactions (register contents stay).
    pub fn clear_log(&mut self) {
        self.log.clear();
        for t in self.targets.values_mut() {
            t.raw_writes.clear();
        }
    }

    /// The next transaction fails with `kind` (queued; one per call).
    pub fn fail_next(&mut self, kind: ErrorKind) {
        self.fail.push_back(kind);
    }

    fn run(&mut self, address: u8, operations: &mut [I2cOperation<'_>]) -> Result<(), ErrorKind> {
        self.log.push(I2cTransfer {
            address,
            ops: operations
                .iter()
                .map(|op| match op {
                    I2cOperation::Write(data) => MockOp::Write(data.to_vec()),
                    I2cOperation::Read(buf) => MockOp::Read(buf.len()),
                })
                .collect(),
        });
        if let Some(kind) = self.fail.pop_front() {
            return Err(kind);
        }
        let target = self.targets.get_mut(&address).ok_or(ErrorKind::Nack)?;
        let mut previous_write = false;
        for op in operations.iter_mut() {
            match op {
                I2cOperation::Write(data) => {
                    let mut data: &[u8] = data;
                    if !previous_write {
                        target.raw_writes.push(Vec::new());
                        if let Some(width) = target.width {
                            let n = width.bytes().min(data.len());
                            target.pointer = data[..n]
                                .iter()
                                .fold(0u16, |acc, b| (acc << 8) | u16::from(*b));
                        }
                    }
                    if let Some(run) = target.raw_writes.last_mut() {
                        run.extend_from_slice(data);
                    }
                    if let Some(width) = target.width {
                        if !previous_write {
                            data = &data[width.bytes().min(data.len())..];
                        }
                        for b in data {
                            target.registers.insert(target.pointer, *b);
                            target.pointer = target.pointer.wrapping_add(1);
                        }
                    }
                    previous_write = true;
                }
                I2cOperation::Read(buf) => {
                    for b in buf.iter_mut() {
                        *b = target.registers.get(&target.pointer).copied().unwrap_or(0);
                        target.pointer = target.pointer.wrapping_add(1);
                    }
                    previous_write = false;
                }
            }
        }
        Ok(())
    }
}

impl I2cErrorType for MockI2c {
    type Error = ErrorKind;
}

impl I2c for MockI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> Result<(), Self::Error> {
        self.run(address, operations)
    }
}

impl embedded_hal_async::i2c::I2c for MockI2c {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> Result<(), Self::Error> {
        self.run(address, operations)
    }
}

/// An SPI device that records transactions and clocks out queued response
/// bytes on reads and transfers (0 when the queue is empty).
#[derive(Debug, Clone, Default)]
pub struct MockSpi {
    /// Every transaction, as given.
    pub transactions: Vec<Vec<MockOp>>,
    /// Bytes the device sends, in order.
    pub responses: VecDeque<u8>,
    fail: VecDeque<ErrorKind>,
}

impl MockSpi {
    /// A device with no queued responses.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues bytes the device sends.
    pub fn with_response(mut self, bytes: &[u8]) -> Self {
        self.responses.extend(bytes.iter().copied());
        self
    }

    /// The next transaction fails with `kind`.
    pub fn fail_next(&mut self, kind: ErrorKind) {
        self.fail.push_back(kind);
    }

    fn next_byte(&mut self) -> u8 {
        self.responses.pop_front().unwrap_or(0)
    }

    fn run(&mut self, operations: &mut [SpiOperation<'_, u8>]) -> Result<(), ErrorKind> {
        if let Some(kind) = self.fail.pop_front() {
            return Err(kind);
        }
        let mut log = Vec::with_capacity(operations.len());
        for op in operations.iter_mut() {
            match op {
                SpiOperation::Write(data) => log.push(MockOp::Write(data.to_vec())),
                SpiOperation::Read(buf) => {
                    log.push(MockOp::Read(buf.len()));
                    for b in buf.iter_mut() {
                        *b = self.next_byte();
                    }
                }
                SpiOperation::Transfer(read, write) => {
                    log.push(MockOp::Transfer(write.to_vec()));
                    for b in read.iter_mut() {
                        *b = self.next_byte();
                    }
                }
                SpiOperation::TransferInPlace(buf) => {
                    log.push(MockOp::Transfer(buf.to_vec()));
                    for b in buf.iter_mut() {
                        *b = self.next_byte();
                    }
                }
                SpiOperation::DelayNs(ns) => log.push(MockOp::DelayNs(*ns)),
            }
        }
        self.transactions.push(log);
        Ok(())
    }
}

impl SpiErrorType for MockSpi {
    type Error = ErrorKind;
}

impl SpiDevice for MockSpi {
    fn transaction(&mut self, operations: &mut [SpiOperation<'_, u8>]) -> Result<(), ErrorKind> {
        self.run(operations)
    }
}

impl embedded_hal_async::spi::SpiDevice for MockSpi {
    async fn transaction(
        &mut self,
        operations: &mut [SpiOperation<'_, u8>],
    ) -> Result<(), ErrorKind> {
        self.run(operations)
    }
}

/// A pin: an output that records every level set, and an input whose level
/// tests set through `input`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockPin {
    /// The level driven (true: high).
    pub output: bool,
    /// The level read as an input.
    pub input: bool,
    /// Every level set, in order.
    pub log: Vec<bool>,
    /// Setting the level fails with this kind.
    pub fail: Option<ErrorKind>,
}

impl MockPin {
    /// A low pin.
    pub fn new() -> Self {
        Self::default()
    }

    fn set(&mut self, high: bool) -> Result<(), ErrorKind> {
        if let Some(kind) = self.fail {
            return Err(kind);
        }
        self.output = high;
        self.log.push(high);
        Ok(())
    }
}

impl PinErrorType for MockPin {
    type Error = ErrorKind;
}

impl OutputPin for MockPin {
    fn set_low(&mut self) -> Result<(), ErrorKind> {
        self.set(false)
    }
    fn set_high(&mut self) -> Result<(), ErrorKind> {
        self.set(true)
    }
}

impl StatefulOutputPin for MockPin {
    fn is_set_high(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.output)
    }
    fn is_set_low(&mut self) -> Result<bool, ErrorKind> {
        Ok(!self.output)
    }
}

impl InputPin for MockPin {
    fn is_high(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.input)
    }
    fn is_low(&mut self) -> Result<bool, ErrorKind> {
        Ok(!self.input)
    }
}

/// A delay that only adds up the time asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MockDelay {
    /// Total nanoseconds waited.
    pub total_ns: u64,
    /// Number of delay calls.
    pub calls: u32,
}

impl MockDelay {
    /// No time waited yet.
    pub fn new() -> Self {
        Self::default()
    }
}

impl embedded_hal::delay::DelayNs for MockDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.total_ns += u64::from(ns);
        self.calls += 1;
    }
}

impl embedded_hal_async::delay::DelayNs for MockDelay {
    async fn delay_ns(&mut self, ns: u32) {
        self.total_ns += u64::from(ns);
        self.calls += 1;
    }
}

/// A regulator that records every switch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockRegulator {
    /// Whether it is on.
    pub enabled: bool,
    /// Every enable (true) and disable (false), in order.
    pub log: Vec<bool>,
    /// The voltage, settable when present.
    pub microvolts: Option<u32>,
}

impl Regulator for MockRegulator {
    type Error = ErrorKind;

    fn enable(&mut self) -> Result<(), ErrorKind> {
        self.enabled = true;
        self.log.push(true);
        Ok(())
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        self.enabled = false;
        self.log.push(false);
        Ok(())
    }

    fn is_enabled(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.enabled)
    }

    fn set_voltage_uv(&mut self, min_uv: u32, _max_uv: u32) -> Result<u32, ErrorKind> {
        match self.microvolts {
            Some(_) => {
                self.microvolts = Some(min_uv);
                Ok(min_uv)
            }
            None => Err(ErrorKind::Unsupported),
        }
    }

    fn voltage_uv(&mut self) -> Result<Option<u32>, ErrorKind> {
        Ok(self.microvolts)
    }
}

/// A clock whose rate can be set freely; records enables and rates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockClock {
    /// The rate in hertz.
    pub rate_hz: u32,
    /// Whether it runs.
    pub enabled: bool,
    /// `Some(rate)` per enable, `None` per disable, in order.
    pub log: Vec<Option<u32>>,
}

impl ClockOutput for MockClock {
    type Error = ErrorKind;

    fn enable(&mut self) -> Result<(), ErrorKind> {
        self.enabled = true;
        self.log.push(Some(self.rate_hz));
        Ok(())
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        self.enabled = false;
        self.log.push(None);
        Ok(())
    }

    fn rate_hz(&mut self) -> Result<u32, ErrorKind> {
        Ok(self.rate_hz)
    }

    fn set_rate_hz(&mut self, rate_hz: u32) -> Result<u32, ErrorKind> {
        if rate_hz == 0 {
            return Err(ErrorKind::InvalidInput);
        }
        self.rate_hz = rate_hz;
        Ok(rate_hz)
    }
}
