//! In-memory doubles for tests (feature `mock`, needs `alloc`): an I2C bus
//! with register-file targets, an SPI device, pins, a delay, a regulator and a
//! clock. Each implements the blocking and the async embedded-hal traits and
//! uses [`ErrorKind`] as its error, so failures can be injected by kind.

extern crate alloc;

#[path = "mock_raw.rs"]
mod raw;
pub use raw::{MockLine, MockPwm, MockRawSpi, SpiRecord};
#[cfg(feature = "std")]
extern crate std;

use crate::ErrorKind;
#[cfg(test)]
use crate::power::{ClockOutput, Regulator};
use crate::register::AddressWidth;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::future::Future;
use core::task::{Context, Poll, Waker};
#[cfg(test)]
use embedded_hal::digital::{ErrorType as PinErrorType, InputPin, OutputPin, StatefulOutputPin};
use embedded_hal::i2c::{ErrorType as I2cErrorType, I2c, Operation as I2cOperation};
#[cfg(test)]
use embedded_hal::spi::{ErrorType as SpiErrorType, Operation as SpiOperation, SpiDevice};

/// Runs a future that completes without waiting on real events to
/// completion (it polls again whenever the future returns `Pending`, as the
/// mocks do when asked to with [`MockI2c::with_pending_polls`]).
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
    /// The target stopped answering: transfers to it are not acknowledged.
    pub dead: bool,
    /// A FIFO behind the data register (see [`MockI2c::with_fifo`]).
    pub fifo: Option<MockFifo>,
    pointer: u16,
}

/// Where a [`MockFifo`] keeps its fill level, so a driver can read how much
/// is waiting before it reads the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FifoLength {
    /// Two bytes at this register, little-endian: the byte count.
    Bytes(u16),
    /// One byte at `register`: the frame count in its low seven bits, with
    /// frames of `frame` bytes.
    Frames { register: u16, frame: usize },
}

/// A FIFO that drains: the bytes the device has buffered wait in a queue, a
/// read of the data register takes them off the front, and the fill level
/// register follows the queue. A read of an empty FIFO returns zeros. The
/// queue is unbounded; a test keeps it within the device's depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockFifo {
    /// The data register: a read starting here takes bytes from the queue.
    pub data: u16,
    /// The fill level register.
    pub length: FifoLength,
    /// The buffered bytes, oldest first.
    pub queue: VecDeque<u8>,
    /// The device's own clock, when it produces frames (see
    /// [`MockI2c::with_fifo_stream`]).
    #[cfg(feature = "std")]
    stream: Option<Stream>,
}

/// A device that buffers one frame per `period`, on the host's clock: frame
/// `i` appears at `origin + i * period`, whether or not anything reads.
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stream {
    origin: std::time::Instant,
    period: core::time::Duration,
    /// Frames produced so far (the next frame's index).
    produced: u64,
    /// One frame's bytes.
    frame: Vec<u8>,
}

impl MockFifo {
    /// Produces the frames the device's clock has reached (a stream only).
    #[cfg(feature = "std")]
    fn produce(&mut self, now: std::time::Instant) {
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        let elapsed = now.saturating_duration_since(stream.origin);
        let due = (elapsed.as_nanos() / stream.period.as_nanos().max(1)) as u64 + 1;
        for _ in stream.produced..due {
            self.queue.extend(stream.frame.iter().copied());
        }
        stream.produced = stream.produced.max(due);
    }

    /// Writes the fill level into the target's registers.
    fn publish_level(&self, registers: &mut BTreeMap<u16, u8>) {
        match self.length {
            FifoLength::Bytes(register) => {
                let [lo, hi] = (self.queue.len() as u16).to_le_bytes();
                registers.insert(register, lo);
                registers.insert(register.wrapping_add(1), hi);
            }
            FifoLength::Frames { register, frame } => {
                let frames = self.queue.len() / frame.max(1);
                registers.insert(register, frames.min(0x7f) as u8);
            }
        }
    }
}

#[derive(Debug, Default)]
struct I2cState {
    targets: BTreeMap<u8, MockI2cTarget>,
    log: Vec<I2cTransfer>,
    fail: VecDeque<ErrorKind>,
    pending_polls: u32,
    /// How long each blocking transaction takes (`std`; see `set_latency`).
    #[cfg(feature = "std")]
    latency: core::time::Duration,
}

/// An I2C bus with register-file targets that auto-increment their register
/// pointer, as most sensors do. Unknown and dead addresses do not
/// acknowledge.
///
/// Clones share the bus: a test keeps one to inspect what a driver did with
/// another. With the `std` feature the state sits behind a mutex (the bus is
/// `Send + Sync`); without it, behind a `RefCell` (single-threaded).
#[derive(Debug, Clone, Default)]
pub struct MockI2c {
    state: Shared<I2cState>,
}

impl MockI2c {
    /// An empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a register-file target with `width` register addresses.
    pub fn with_target(self, address: u8, width: AddressWidth) -> Self {
        with(&self.state, |s| {
            s.targets.insert(
                address,
                MockI2cTarget {
                    width: Some(width),
                    ..MockI2cTarget::default()
                },
            )
        });
        self
    }

    /// Adds a target without registers (e.g. a DW9714 VCM).
    pub fn with_raw_target(self, address: u8) -> Self {
        with(&self.state, |s| {
            s.targets.insert(address, MockI2cTarget::default())
        });
        self
    }

    /// Presets consecutive registers of a target.
    pub fn with_registers(self, address: u8, register: u16, bytes: &[u8]) -> Self {
        with(&self.state, |s| {
            let target = s.targets.entry(address).or_default();
            for (i, b) in bytes.iter().enumerate() {
                target.registers.insert(register.wrapping_add(i as u16), *b);
            }
        });
        self
    }

    /// Async transfers return `Pending` this many times (waking themselves)
    /// before completing, so callers are tested across await points.
    pub fn with_pending_polls(self, polls: u32) -> Self {
        self.set_pending_polls(polls);
        self
    }

    /// Sets how many times async transfers return `Pending` first.
    pub fn set_pending_polls(&self, polls: u32) {
        with(&self.state, |s| s.pending_polls = polls);
    }

    /// Makes the target at `address` stop (or resume) acknowledging, like a
    /// sensor that lost power or was unplugged. Its registers are kept.
    pub fn set_dead(&self, address: u8, dead: bool) {
        with(&self.state, |s| {
            s.targets.entry(address).or_default().dead = dead
        });
    }

    /// Gives the register-file target at `address` a FIFO behind `data`
    /// (see [`MockFifo`]). It starts empty; the device's output comes in with
    /// [`push_fifo`](Self::push_fifo).
    pub fn with_fifo(self, address: u8, data: u16, length: FifoLength) -> Self {
        with(&self.state, |s| {
            s.targets.entry(address).or_default().fifo = Some(MockFifo {
                data,
                length,
                queue: VecDeque::new(),
                #[cfg(feature = "std")]
                stream: None,
            });
        });
        self
    }

    /// Gives the target at `address` a FIFO that the device fills on the
    /// host's clock: one `frame` (the same bytes each time) is buffered every
    /// `period` from this call, so a read takes the frames that have arrived
    /// since the last one, as a real FIFO does. The level register follows the
    /// queue.
    #[cfg(feature = "std")]
    pub fn with_fifo_stream(
        self,
        address: u8,
        data: u16,
        length: FifoLength,
        period: core::time::Duration,
        frame: &[u8],
    ) -> Self {
        with(&self.state, |s| {
            s.targets.entry(address).or_default().fifo = Some(MockFifo {
                data,
                length,
                queue: VecDeque::new(),
                stream: Some(Stream {
                    origin: std::time::Instant::now(),
                    period,
                    produced: 0,
                    frame: frame.to_vec(),
                }),
            });
        });
        self
    }

    /// Appends the device's output to the FIFO at `address` (the sensor
    /// buffering frames). Panics without a FIFO.
    pub fn push_fifo(&self, address: u8, bytes: &[u8]) {
        with(&self.state, |s| {
            let target = s.targets.entry(address).or_default();
            let fifo = target.fifo.as_mut().expect("no FIFO on this target");
            fifo.queue.extend(bytes.iter().copied());
            fifo.publish_level(&mut target.registers);
        });
    }

    /// Bytes waiting in the FIFO at `address` (0 without one).
    pub fn fifo_bytes(&self, address: u8) -> usize {
        with(&self.state, |s| {
            s.targets
                .get(&address)
                .and_then(|t| t.fifo.as_ref())
                .map_or(0, |f| f.queue.len())
        })
    }

    /// The target at `address` (a snapshot).
    pub fn target(&self, address: u8) -> Option<MockI2cTarget> {
        with(&self.state, |s| s.targets.get(&address).cloned())
    }

    /// A register's value (0 if unset or no such target).
    pub fn register(&self, address: u8, register: u16) -> u8 {
        with(&self.state, |s| {
            s.targets
                .get(&address)
                .and_then(|t| t.registers.get(&register).copied())
                .unwrap_or(0)
        })
    }

    /// `bytes` (1 to 4) consecutive registers from `register` as one value,
    /// most significant byte first.
    pub fn value(&self, address: u8, register: u16, bytes: u8) -> u32 {
        (0..u16::from(bytes.min(4))).fold(0u32, |acc, i| {
            (acc << 8) | u32::from(self.register(address, register.wrapping_add(i)))
        })
    }

    /// Every transaction so far (a snapshot).
    pub fn transfers(&self) -> Vec<I2cTransfer> {
        with(&self.state, |s| s.log.clone())
    }

    /// Forgets recorded transactions (register contents stay).
    pub fn clear_log(&self) {
        with(&self.state, |s| {
            s.log.clear();
            for t in s.targets.values_mut() {
                t.raw_writes.clear();
            }
        });
    }

    /// The next transaction fails with `kind` (queued; one per call).
    pub fn fail_next(&self, kind: ErrorKind) {
        with(&self.state, |s| s.fail.push_back(kind));
    }

    fn run(&self, address: u8, operations: &mut [I2cOperation<'_>]) -> Result<(), ErrorKind> {
        with(&self.state, |s| s.run(address, operations))
    }
}

impl I2cState {
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
        let target = self
            .targets
            .get_mut(&address)
            .filter(|t| !t.dead)
            .ok_or(ErrorKind::Nack)?;
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
                    #[cfg(feature = "std")]
                    if let Some(fifo) = target.fifo.as_mut() {
                        fifo.produce(std::time::Instant::now());
                        fifo.publish_level(&mut target.registers);
                    }
                    match target.fifo.as_mut().filter(|f| f.data == target.pointer) {
                        // A read of the data register takes what it reads.
                        Some(fifo) => {
                            for b in buf.iter_mut() {
                                *b = fifo.queue.pop_front().unwrap_or(0);
                            }
                            fifo.publish_level(&mut target.registers);
                        }
                        None => {
                            for b in buf.iter_mut() {
                                *b = target.registers.get(&target.pointer).copied().unwrap_or(0);
                                target.pointer = target.pointer.wrapping_add(1);
                            }
                        }
                    }
                    previous_write = false;
                }
            }
        }
        Ok(())
    }
}

impl MockI2c {
    /// Makes every blocking transaction on this bus take `latency` (a slow
    /// device on a real bus, for scheduling tests). Shared with the clones
    /// of this bus; the async transactions are not delayed.
    #[cfg(feature = "std")]
    pub fn set_latency(&self, latency: core::time::Duration) {
        with(&self.state, |s| s.latency = latency);
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
        #[cfg(feature = "std")]
        {
            let latency = with(&self.state, |s| s.latency);
            if !latency.is_zero() {
                std::thread::sleep(latency);
            }
        }
        self.run(address, operations)
    }
}

impl embedded_hal_async::i2c::I2c for MockI2c {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> Result<(), Self::Error> {
        yield_times(with(&self.state, |s| s.pending_polls)).await;
        self.run(address, operations)
    }
}

/// Returns `Pending` `n` times (waking itself each time), then `Ready`.
async fn yield_times(n: u32) {
    let mut left = n;
    core::future::poll_fn(|cx| {
        if left == 0 {
            Poll::Ready(())
        } else {
            left -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

/// State that clones of a mock share.
#[cfg(feature = "std")]
type Shared<T> = alloc::sync::Arc<std::sync::Mutex<T>>;
#[cfg(not(feature = "std"))]
type Shared<T> = alloc::rc::Rc<core::cell::RefCell<T>>;

#[cfg(feature = "std")]
fn with<T, R>(shared: &Shared<T>, f: impl FnOnce(&mut T) -> R) -> R {
    let mut guard = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

#[cfg(not(feature = "std"))]
fn with<T, R>(shared: &Shared<T>, f: impl FnOnce(&mut T) -> R) -> R {
    f(&mut shared.borrow_mut())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::I2cRegisters;
    use crate::asynch::{Blocking, RegisterBus as _};
    use embedded_hal_async::i2c::I2c as AsyncI2c;

    #[cfg(feature = "std")]
    #[test]
    fn the_std_bus_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<MockI2c>();
    }

    #[test]
    fn clones_share_the_bus() {
        let bus = MockI2c::new().with_target(0x18, AddressWidth::Bits8);
        let mut regs = I2cRegisters::new(bus.clone(), 0x18, AddressWidth::Bits8);
        crate::RegisterBus::write(&mut regs, 0x10, 2, 0xbeef).unwrap();
        assert_eq!(bus.value(0x18, 0x10, 2), 0xbeef);
        assert_eq!(bus.transfers().len(), 1);
        bus.clear_log();
        assert!(crate::RegisterBus::read(&mut regs, 0x10, 1).is_ok());
        assert_eq!(bus.transfers().len(), 1);
    }

    #[test]
    fn pending_polls_suspend_async_transfers() {
        let mut bus = MockI2c::new()
            .with_registers(0x10, 0x00, &[0x32])
            .with_pending_polls(3);
        let mut polls = 0;
        let mut buf = [0u8; 1];
        {
            let mut future =
                core::pin::pin!(AsyncI2c::write_read(&mut bus, 0x10, &[0x00], &mut buf));
            let mut cx = Context::from_waker(Waker::noop());
            while future.as_mut().poll(&mut cx).is_pending() {
                polls += 1;
            }
        }
        assert_eq!((polls, buf), (3, [0x32]));
        bus.set_pending_polls(0);
        assert_eq!(block_on(AsyncI2c::write(&mut bus, 0x10, &[0x00])), Ok(()));
    }

    #[test]
    fn a_fifo_read_takes_what_it_reads() {
        let bus = MockI2c::new()
            .with_target(0x18, AddressWidth::Bits8)
            .with_fifo(0x18, 0x26, FifoLength::Bytes(0x24));
        bus.push_fifo(0x18, &[1, 2, 3, 4, 5, 6, 7]);
        let mut regs = I2cRegisters::new(bus.clone(), 0x18, AddressWidth::Bits8);
        // The fill level reads as the byte count, little-endian.
        let mut level = [0u8; 2];
        crate::RegisterBus::read_burst(&mut regs, 0x24, &mut level).unwrap();
        assert_eq!(u16::from_le_bytes(level), 7);
        let mut first = [0u8; 3];
        crate::RegisterBus::read_burst(&mut regs, 0x26, &mut first).unwrap();
        assert_eq!(first, [1, 2, 3]);
        // The next read continues where the last stopped, not from the top.
        let mut rest = [0u8; 5];
        crate::RegisterBus::read_burst(&mut regs, 0x26, &mut rest).unwrap();
        assert_eq!(rest, [4, 5, 6, 7, 0]);
        assert_eq!(bus.fifo_bytes(0x18), 0);
        assert_eq!(bus.value(0x18, 0x24, 2), 0);
    }

    #[cfg(feature = "std")]
    #[test]
    fn a_streaming_fifo_buffers_frames_on_the_device_clock() {
        let bus = MockI2c::new()
            .with_target(0x18, AddressWidth::Bits8)
            .with_fifo_stream(
                0x18,
                0x26,
                FifoLength::Bytes(0x24),
                core::time::Duration::from_millis(1),
                &[0x84, 1, 0, 2, 0, 3, 0],
            );
        // Nothing read yet: the clock still runs, so frames have arrived.
        std::thread::sleep(core::time::Duration::from_millis(20));
        let mut regs = I2cRegisters::new(bus.clone(), 0x18, AddressWidth::Bits8);
        let mut level = [0u8; 2];
        crate::RegisterBus::read_burst(&mut regs, 0x24, &mut level).unwrap();
        let waiting = usize::from(u16::from_le_bytes(level));
        // Seven bytes a frame: about 20 frames in 20 ms at 1 ms a frame.
        assert!((105..=175).contains(&waiting), "{waiting} bytes after 20 ms");
        // The data read takes them, oldest first, and leaves the rest.
        let mut taken = alloc::vec![0u8; waiting];
        crate::RegisterBus::read_burst(&mut regs, 0x26, &mut taken).unwrap();
        assert!(taken.chunks(7).all(|f| f == [0x84, 1, 0, 2, 0, 3, 0]));
        assert!(bus.fifo_bytes(0x18) < 10);
    }

    #[test]
    fn a_frame_fifo_reports_its_frame_count() {
        let bus = MockI2c::new()
            .with_target(0x68, AddressWidth::Bits8)
            .with_fifo(0x68, 0x3f, FifoLength::Frames { register: 0x0e, frame: 6 });
        bus.push_fifo(0x68, &[0; 12]);
        assert_eq!(bus.register(0x68, 0x0e), 2);
        let mut regs = I2cRegisters::new(bus.clone(), 0x68, AddressWidth::Bits8);
        crate::RegisterBus::read_burst(&mut regs, 0x3f, &mut [0u8; 6]).unwrap();
        assert_eq!(bus.register(0x68, 0x0e), 1);
    }

    #[test]
    fn dead_targets_do_not_acknowledge() {
        let bus = MockI2c::new()
            .with_target(0x0c, AddressWidth::Bits8)
            .with_registers(0x0c, 0x01, &[7]);
        let mut regs = Blocking(I2cRegisters::new(bus.clone(), 0x0c, AddressWidth::Bits8));
        assert_eq!(block_on(regs.read8(0x01)), Ok(7));
        bus.set_dead(0x0c, true);
        assert!(block_on(regs.read8(0x01)).is_err());
        bus.set_dead(0x0c, false);
        assert_eq!(block_on(regs.read8(0x01)), Ok(7));
        assert!(bus.target(0x0c).is_some_and(|t| !t.dead));
    }
}

#[cfg(test)]
mod raw_tests {
    use super::*;
    use crate::raw::{
        Direction, EdgeDetect, LineConfig, PwmConfig, RawLine, RawPwm, RawSpi, SafeState,
        SpiConfig, SpiMode, SpiSegment,
    };

    #[test]
    fn mock_line_reads_drives_and_reports_edges() {
        let line = MockLine::new();
        let mut claimed = line.clone();
        claimed
            .configure(&LineConfig::input().with_edge(EdgeDetect::Both))
            .unwrap();
        line.drive(true);
        assert_eq!(claimed.get(), Ok(true));
        line.edge(false, 10);
        assert_eq!(claimed.get(), Ok(false));
        let edge = claimed.read_edge().unwrap().unwrap();
        assert!(!edge.rising && edge.timestamp_ns == 10 && edge.seq == 1);
        assert_eq!(claimed.read_edge(), Ok(None));
        assert_eq!(claimed.set(true), Err(ErrorKind::InvalidInput));
        claimed
            .configure(&LineConfig::output(true).active_low())
            .unwrap();
        assert_eq!(line.level(), Some(false));
        claimed.set(false).unwrap();
        assert_eq!(line.level(), Some(true));
        claimed
            .configure(&SafeState::Input.config().unwrap())
            .unwrap();
        assert_eq!(line.config().direction, Direction::Input);
        assert_eq!(line.configs().len(), 3);
        assert_eq!(SafeState::Keep.config(), None);
        assert_eq!(SafeState::parse("high"), Some(SafeState::High));
    }

    #[test]
    fn mock_pwm_and_spi_record() {
        let pwm = MockPwm::new();
        let mut claimed = pwm.clone();
        let config = PwmConfig {
            period_ns: 1_000_000,
            duty_ns: 250_000,
            enabled: true,
            ..PwmConfig::default()
        };
        claimed.configure(&config).unwrap();
        assert_eq!(claimed.config(), Ok(config));
        assert!(
            claimed
                .configure(&PwmConfig {
                    duty_ns: 2,
                    period_ns: 1,
                    ..config
                })
                .is_err()
        );
        assert_eq!(pwm.history(), [config]);

        let spi = MockRawSpi::new();
        spi.respond(&[0xaa, 0xbb]);
        let mut device = spi.clone();
        let mut rx = [0u8; 2];
        let config = SpiConfig {
            mode: SpiMode::from_bits(3),
            speed_hz: 1_000_000,
            bits_per_word: 8,
        };
        device
            .transfer(&mut [SpiSegment {
                config,
                tx: &[0x9f, 0],
                rx: &mut rx,
                cs_change: false,
                delay_us: 0,
            }])
            .unwrap();
        assert_eq!(rx, [0xaa, 0xbb]);
        assert_eq!(spi.transactions()[0][0].tx, [0x9f, 0]);
        assert_eq!(spi.transactions()[0][0].config.mode.bits(), 3);
    }
}

/// An SPI device that records transactions and clocks out queued response
/// bytes on reads and transfers (0 when the queue is empty).
#[derive(Debug, Clone, Default)]
#[cfg(test)]
pub(crate) struct MockSpi {
    /// Every transaction, as given.
    pub transactions: Vec<Vec<MockOp>>,
    /// Bytes the device sends, in order.
    pub responses: VecDeque<u8>,
}

#[cfg(test)]
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

    fn next_byte(&mut self) -> u8 {
        self.responses.pop_front().unwrap_or(0)
    }

    fn run(&mut self, operations: &mut [SpiOperation<'_, u8>]) -> Result<(), ErrorKind> {
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

#[cfg(test)]
impl SpiErrorType for MockSpi {
    type Error = ErrorKind;
}

#[cfg(test)]
impl SpiDevice for MockSpi {
    fn transaction(&mut self, operations: &mut [SpiOperation<'_, u8>]) -> Result<(), ErrorKind> {
        self.run(operations)
    }
}

#[cfg(test)]
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
#[cfg(test)]
pub(crate) struct MockPin {
    /// The level driven (true: high).
    pub output: bool,
    /// The level read as an input.
    pub input: bool,
    /// Every level set, in order.
    pub log: Vec<bool>,
    /// Setting the level fails with this kind.
    pub fail: Option<ErrorKind>,
}

#[cfg(test)]
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

#[cfg(test)]
impl PinErrorType for MockPin {
    type Error = ErrorKind;
}

#[cfg(test)]
impl OutputPin for MockPin {
    fn set_low(&mut self) -> Result<(), ErrorKind> {
        self.set(false)
    }
    fn set_high(&mut self) -> Result<(), ErrorKind> {
        self.set(true)
    }
}

#[cfg(test)]
impl StatefulOutputPin for MockPin {
    fn is_set_high(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.output)
    }
    fn is_set_low(&mut self) -> Result<bool, ErrorKind> {
        Ok(!self.output)
    }
}

#[cfg(test)]
impl InputPin for MockPin {
    fn is_high(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.input)
    }
    fn is_low(&mut self) -> Result<bool, ErrorKind> {
        Ok(!self.input)
    }
}

/// A regulator that records every switch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct MockRegulator {
    /// Whether it is on.
    pub enabled: bool,
    /// Every enable (true) and disable (false), in order.
    pub log: Vec<bool>,
    /// The voltage, settable when present.
    pub microvolts: Option<u32>,
}

#[cfg(test)]
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
#[cfg(test)]
pub(crate) struct MockClock {
    /// The rate in hertz.
    pub rate_hz: u32,
    /// Whether it runs.
    pub enabled: bool,
    /// `Some(rate)` per enable, `None` per disable, in order.
    pub log: Vec<Option<u32>>,
}

#[cfg(test)]
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
