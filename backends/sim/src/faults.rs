//! Fault injection: every simulated resource is a [`Faulty`] over a mock.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lemnos_core::ErrorKind;
use lemnos_hal::blocks::{
    BufferHandle, BufferKind, Clocks, ControlSetup, DmaBuffer, DmaBuffers, Interrupt,
    InterruptEvent, IsoPacket, Registers, Reset, StreamEndpoint, StreamFrame, Uart, UsbDevice,
    UsbFunction, UsbFunctionEvent,
};
use lemnos_hal::i2c::{self, I2c, Operation};
use lemnos_hal::raw::{Edge, LineConfig, PwmConfig, RawLine, RawPwm, RawSpi, SpiSegment};
use lemnos_hal::spi::{self, SpiDevice};

use crate::SimClock;

/// One planned failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// The operation it hits, or any operation.
    pub operation: Option<&'static str>,
    pub error: ErrorKind,
}

#[derive(Debug)]
struct Outage {
    from_ns: u64,
    until_ns: Option<u64>,
    error: ErrorKind,
}

#[derive(Debug, Default)]
struct ScriptState {
    queued: VecDeque<Fault>,
    outages: Vec<Outage>,
    clock: Option<SimClock>,
}

/// Which operations fail. Clones share the script.
#[derive(Debug, Clone, Default)]
pub struct FaultScript(Arc<Mutex<ScriptState>>);

impl FaultScript {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn on_clock(clock: SimClock) -> Self {
        let script = Self::default();
        script.with(|s| s.clock = Some(clock));
        script
    }

    fn with<R>(&self, f: impl FnOnce(&mut ScriptState) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The next operation fails with `error`, once.
    pub fn fail_next(&self, error: ErrorKind) {
        self.with(|s| {
            s.queued.push_back(Fault {
                operation: None,
                error,
            })
        });
    }

    /// The next `operation` fails with `error`, once. Faults queued earlier
    /// wait their turn, so a script replays in order.
    pub fn fail_next_op(&self, operation: &'static str, error: ErrorKind) {
        self.with(|s| {
            s.queued.push_back(Fault {
                operation: Some(operation),
                error,
            })
        });
    }

    /// Every operation fails with `error` from `from` for `lasting`, or for
    /// good when that is `None`. Needs a script made by a [`crate::SimBoard`].
    pub fn outage(&self, from: Duration, lasting: Option<Duration>, error: ErrorKind) {
        let from_ns = from.as_nanos() as u64;
        self.with(|s| {
            s.outages.push(Outage {
                from_ns,
                until_ns: lasting.map(|d| from_ns + d.as_nanos() as u64),
                error,
            })
        });
    }

    /// Drops everything planned.
    pub fn clear(&self) {
        self.with(|s| {
            s.queued.clear();
            s.outages.clear();
        });
    }

    pub(crate) fn check(&self, operation: &'static str) -> Result<(), ErrorKind> {
        self.with(|s| {
            if let Some(clock) = &s.clock {
                let now = clock.now_ns();
                let down = s
                    .outages
                    .iter()
                    .find(|o| now >= o.from_ns && o.until_ns.is_none_or(|end| now < end));
                if let Some(outage) = down {
                    return Err(outage.error);
                }
            }
            let hit = s
                .queued
                .front()
                .is_some_and(|f| f.operation.is_none_or(|op| op == operation));
            if hit && let Some(fault) = s.queued.pop_front() {
                return Err(fault.error);
            }
            Ok(())
        })
    }
}

/// Runs its closure when dropped; holds an exclusive resource.
pub(crate) struct Release(pub Option<Box<dyn FnOnce() + Send>>);

impl Drop for Release {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            release();
        }
    }
}

/// A resource that consults a [`FaultScript`] before each operation.
pub struct Faulty<T> {
    pub(crate) inner: T,
    pub(crate) faults: FaultScript,
    pub(crate) _release: Release,
}

impl<T> Faulty<T> {
    pub(crate) fn new(inner: T, faults: FaultScript, release: Release) -> Self {
        Self {
            inner,
            faults,
            _release: release,
        }
    }
}

macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {$(
        fn $name(&mut self, $($arg: $ty),*) -> Result<$ret, ErrorKind> {
            self.faults.check(stringify!($name))?;
            self.inner.$name($($arg),*)
        }
    )*};
}

impl<T: I2c<Error = ErrorKind>> i2c::ErrorType for Faulty<T> {
    type Error = ErrorKind;
}

impl<T: I2c<Error = ErrorKind>> I2c for Faulty<T> {
    forward! { transaction(address: u8, operations: &mut [Operation<'_>]) -> (); }
}

impl<T: SpiDevice<Error = ErrorKind>> spi::ErrorType for Faulty<T> {
    type Error = ErrorKind;
}

impl<T: SpiDevice<Error = ErrorKind>> SpiDevice for Faulty<T> {
    forward! { transaction(operations: &mut [spi::Operation<'_, u8>]) -> (); }
}

impl<T: RawSpi> RawSpi for Faulty<T> {
    forward! { transfer(segments: &mut [SpiSegment<'_>]) -> (); }
}

impl<T: RawLine> RawLine for Faulty<T> {
    forward! {
        configure(config: &LineConfig) -> ();
        get() -> bool;
        set(value: bool) -> ();
        read_edge() -> Option<Edge>;
    }
}

impl<T: RawPwm> RawPwm for Faulty<T> {
    forward! {
        configure(config: &PwmConfig) -> ();
        config() -> PwmConfig;
    }
}

impl<T: Uart<Error = ErrorKind>> Uart for Faulty<T> {
    type Error = ErrorKind;
    forward! {
        set_baud(baud: u32) -> ();
        write(data: &[u8]) -> usize;
        read(out: &mut [u8], timeout: Duration) -> usize;
    }
}

impl<T: Registers<Error = ErrorKind>> Registers for Faulty<T> {
    type Error = ErrorKind;

    fn size(&self) -> usize {
        self.inner.size()
    }

    forward! {
        read32(offset: usize) -> u32;
        write32(offset: usize, value: u32) -> ();
    }
}

impl<T: Interrupt<Error = ErrorKind>> Interrupt for Faulty<T> {
    type Error = ErrorKind;
    forward! {
        enable() -> ();
        disable() -> ();
        wait(timeout: Option<Duration>) -> Option<InterruptEvent>;
    }
}

impl<T: DmaBuffers<Error = ErrorKind>> DmaBuffers for Faulty<T> {
    type Error = ErrorKind;
    forward! {
        allocate(len: u64) -> DmaBuffer;
        release(id: u32) -> ();
        write(id: u32, offset: u64, data: &[u8]) -> ();
        read(id: u32, offset: u64, out: &mut [u8]) -> ();
        sync_for_device(id: u32) -> ();
        sync_for_cpu(id: u32) -> ();
    }
}

impl<T: Clocks<Error = ErrorKind>> Clocks for Faulty<T> {
    type Error = ErrorKind;
    forward! {
        enable(clock: u32) -> ();
        disable(clock: u32) -> ();
        rate_hz(clock: u32) -> u64;
        set_rate_hz(clock: u32, hz: u64) -> u64;
    }
}

impl<T: Reset<Error = ErrorKind>> Reset for Faulty<T> {
    type Error = ErrorKind;
    forward! {
        assert(line: u32) -> ();
        deassert(line: u32) -> ();
    }
}

impl<T: StreamEndpoint<Error = ErrorKind>> StreamEndpoint for Faulty<T> {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.inner.buffer_count()
    }

    fn buffer_kind(&self) -> BufferKind {
        self.inner.buffer_kind()
    }

    forward! {
        publish(frame: StreamFrame) -> ();
        next_buffer() -> Option<u32>;
        next_job(out: &mut [u8]) -> Option<usize>;
        write(buffer: u32, offset: usize, data: &[u8]) -> bool;
        read(buffer: u32, offset: usize, out: &mut [u8]) -> bool;
        fault_consumer() -> ();
    }

    fn buffer_handle(&self, buffer: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        self.faults.check("buffer_handle")?;
        self.inner.buffer_handle(buffer)
    }
}

impl<T: UsbDevice<Error = ErrorKind>> UsbDevice for Faulty<T> {
    type Error = ErrorKind;
    forward! {
        config_descriptor(index: u8, out: &mut [u8]) -> usize;
        reset() -> ();
        detach_kernel_driver(interface: u8) -> ();
        claim_interface(interface: u8) -> ();
        release_interface(interface: u8) -> ();
        set_alternate(interface: u8, alternate: u8) -> ();
        control_in(setup: ControlSetup, data: &mut [u8], timeout: Duration) -> usize;
        control_out(setup: ControlSetup, data: &[u8], timeout: Duration) -> usize;
        bulk_in(endpoint: u8, data: &mut [u8], timeout: Duration) -> usize;
        bulk_out(endpoint: u8, data: &[u8], timeout: Duration) -> usize;
        interrupt_in(endpoint: u8, data: &mut [u8], timeout: Duration) -> usize;
        iso_in(endpoint: u8, data: &mut [u8], packets: &mut [IsoPacket], timeout: Duration) -> ();
    }
}

impl<T: UsbFunction<Error = ErrorKind>> UsbFunction for Faulty<T> {
    type Error = ErrorKind;
    forward! {
        enable(descriptors: &[u8]) -> ();
        disable() -> ();
        next_event() -> Option<UsbFunctionEvent>;
        control_in(data: &[u8], timeout: Duration) -> usize;
        control_out(data: &mut [u8], timeout: Duration) -> usize;
        control_stall() -> ();
        set_halt(endpoint: u8, halted: bool) -> ();
        read(endpoint: u8, data: &mut [u8], timeout: Duration) -> usize;
        write(endpoint: u8, data: &[u8], timeout: Duration) -> usize;
    }
}
