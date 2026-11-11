//! A whole driver package: a contract, a `#[driver]` type that serves it, and the one line that
//! exports it. The driver is written exactly as a built-in one is.
//!
//! It has no hardware behind it. Its methods are what the loader's tests need: a value that
//! comes back right, a panic, a hang, a call to another device, a read from a bus, and one
//! (`exercise`) that uses every other kind of resource a driver can be given.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::package::status_of_bus;
use lemnos_core::{
    ClocksHandle, Config, Description, Dev, DmaHandle, ErrorKind, Health, I2c, InterruptHandle,
    Item, Lifecycle, Line, MethodId, Pwm, RawSpiBus, RegistersHandle, ResetHandle, Result, Spi,
    Status, StreamHandle, Uart, UsbDeviceHandle, UsbFunctionHandle, contract, driver, status_of,
};
use lemnos_hal::blocks::{BufferKind, ControlSetup, IsoPacket, StreamFrame, UsbFunctionEvent};
use lemnos_hal::i2c::I2c as _;
use lemnos_hal::raw::{
    Bias, EdgeDetect, LineConfig, Polarity, PwmConfig, SpiConfig, SpiMode, SpiSegment,
};
use lemnos_hal::spi::{Operation, SpiDevice as _};
use lemnos_measure::test_alloc::{Counting, TOTAL};

/// Test support, not part of a package: lets a test ask how much this package allocated.
#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[contract("example.package", version = 1)]
pub trait Example {
    /// Counts up from one.
    #[method(1)]
    fn count(&mut self) -> Result<u32>;
    #[method(2)]
    fn add(&mut self, a: u32, b: u32) -> Result<u32>;
    /// Panics.
    #[method(3)]
    fn crash(&mut self) -> Result<()>;
    /// Does not come back for this long.
    #[method(4)]
    fn hang(&mut self, millis: u32) -> Result<()>;
    /// How many allocations this package has made, on any thread.
    #[method(5)]
    fn allocations(&mut self) -> Result<u64>;
    /// The `count` of the device this one uses as `peer`.
    #[method(6)]
    fn peer_count(&mut self) -> Result<u32>;
    /// One register of a chip on the bus this one uses as `bus`.
    #[method(7)]
    fn register(&mut self, address: u8, register: u8) -> Result<u8>;
    /// From now on this driver says what it was bound with is gone.
    #[method(8)]
    fn leave(&mut self) -> Result<()>;
    /// How many times the host has ticked this driver.
    #[method(9)]
    fn ticks(&mut self) -> Result<u32>;
    /// Does one thing (`step`) with the resource of one kind (`kind`) this device was given:
    /// `data` goes in, what came back is written to `into`, and its length returned. The kinds
    /// and steps are listed at [`kind`] and in `exercise.rs`.
    /// How the device this one uses as `peer` is doing: 0 ok, 1 degraded, 2 failed, 3 gone.
    #[method(11)]
    fn peer_health(&mut self) -> Result<u32>;
    #[method(10)]
    fn exercise(
        &mut self,
        kind: u8,
        step: u8,
        data: &[u8],
        #[out] into: &mut [u8],
    ) -> Result<usize>;
}

/// The kinds of resource `exercise` takes.
pub mod kind {
    pub const SPI: u8 = 2;
    pub const LINE: u8 = 3;
    pub const PWM: u8 = 4;
    pub const UART: u8 = 5;
    pub const USB_DEVICE: u8 = 6;
    pub const USB_FUNCTION: u8 = 7;
    pub const REGISTERS: u8 = 8;
    pub const INTERRUPT: u8 = 9;
    pub const DMA: u8 = 10;
    pub const CLOCKS: u8 = 11;
    pub const RESET: u8 = 12;
    pub const STREAM: u8 = 13;
    pub const RAW_SPI: u8 = 14;
}

mod exercise;

/// Binds that have panicked, in this package.
static PANICS: AtomicU32 = AtomicU32::new(0);

#[driver("package-fixture", provides = [Example])]
pub struct Fixture {
    #[needs(optional)]
    peer: Option<Dev<dyn Example>>,
    #[needs(optional)]
    bus: Option<I2c>,
    #[needs(optional)]
    spi: Option<Spi>,
    #[needs(optional)]
    raw_spi: Option<RawSpiBus>,
    #[needs(optional)]
    line: Option<Line>,
    #[needs(optional)]
    pwm: Option<Pwm>,
    #[needs(optional)]
    uart: Option<Uart>,
    #[needs(optional)]
    usb: Option<UsbDeviceHandle>,
    #[needs(optional)]
    function: Option<UsbFunctionHandle>,
    #[needs(optional)]
    registers: Option<RegistersHandle>,
    #[needs(optional)]
    interrupt: Option<InterruptHandle>,
    #[needs(optional)]
    dma: Option<DmaHandle>,
    #[needs(optional)]
    clocks: Option<ClocksHandle>,
    #[needs(optional)]
    reset: Option<ResetHandle>,
    #[needs(optional)]
    stream: Option<StreamHandle>,
    /// The DMA buffer and the stream buffer `exercise` took last.
    dma_buffer: u32,
    stream_buffer: u32,
    count: u32,
    ticks: u32,
    gone: bool,
}

impl Lifecycle for Fixture {
    type Needs = FixtureNeeds;

    fn start(needs: FixtureNeeds, cfg: &Config<'_>) -> Result<Self> {
        // `panic_binds = N` in the board file: the first N binds panic.
        let panics = cfg.int("panic_binds").unwrap_or(0) as u32;
        if panics > 0 && PANICS.fetch_add(1, Ordering::Relaxed) < panics {
            panic!("deliberate panic in bind");
        }
        Ok(Self {
            peer: needs.peer,
            bus: needs.bus,
            spi: needs.spi,
            raw_spi: needs.raw_spi,
            line: needs.line,
            pwm: needs.pwm,
            uart: needs.uart,
            usb: needs.usb,
            function: needs.function,
            registers: needs.registers,
            interrupt: needs.interrupt,
            dma: needs.dma,
            clocks: needs.clocks,
            reset: needs.reset,
            stream: needs.stream,
            dma_buffer: 0,
            stream_buffer: 0,
            count: cfg.int("start").unwrap_or(0) as u32,
            ticks: 0,
            gone: false,
        })
    }

    fn describe(&self, out: &mut Description) {
        let id = |method| MethodId {
            contract: 0,
            method,
        };
        out.add(Item::value("count", id(1)).range(0, i64::from(u32::MAX)));
        out.add(Item::action("crash", id(3)));
    }

    fn health(&self) -> Health {
        if self.gone {
            Health::Gone("asked to leave")
        } else {
            Health::Ok
        }
    }

    const TICKS: bool = true;

    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        self.ticks += 1;
        None
    }
}

impl Example for Fixture {
    fn count(&mut self) -> Result<u32> {
        self.count += 1;
        Ok(self.count)
    }

    fn add(&mut self, a: u32, b: u32) -> Result<u32> {
        a.checked_add(b).ok_or(Status::INVALID_ARGUMENT)
    }

    fn crash(&mut self) -> Result<()> {
        panic!("deliberate panic in a method");
    }

    fn hang(&mut self, millis: u32) -> Result<()> {
        std::thread::sleep(Duration::from_millis(u64::from(millis)));
        Ok(())
    }

    fn allocations(&mut self) -> Result<u64> {
        Ok(TOTAL.load(Ordering::Relaxed) as u64)
    }

    fn peer_count(&mut self) -> Result<u32> {
        self.peer.as_mut().ok_or(Status::NOT_FOUND)?.count()
    }

    fn register(&mut self, address: u8, register: u8) -> Result<u8> {
        let bus = self.bus.as_mut().ok_or(Status::NOT_FOUND)?;
        let mut value = [0];
        bus.write_read(address, &[register], &mut value)
            .map_err(status_of)?;
        Ok(value[0])
    }

    fn leave(&mut self) -> Result<()> {
        self.gone = true;
        Ok(())
    }

    fn ticks(&mut self) -> Result<u32> {
        Ok(self.ticks)
    }

    fn peer_health(&mut self) -> Result<u32> {
        Ok(
            match self.peer.as_ref().ok_or(Status::NOT_FOUND)?.health() {
                Health::Ok => 0,
                Health::Degraded(_) => 1,
                Health::Failed(_) => 2,
                Health::Gone(_) => 3,
            },
        )
    }

    fn exercise(&mut self, kind: u8, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        self.run(kind, step, data, into)
    }
}

// The whole of what makes this crate a package.
#[cfg(not(feature = "panics-registering"))]
lemnos_core::export_package!(Fixture);

/// A package whose registration panics, written out by hand where `export_package!` would be.
#[cfg(feature = "panics-registering")]
mod panics_registering {
    use lemnos_abi::package::{Out, PackageDescriptor, PackageDriver, Slice};
    use lemnos_abi::{Status, guard};

    extern "C" fn register(_driver: Out<'_, Slice<'static, PackageDriver>>) -> Status {
        guard(|| panic!("deliberate panic in registration"))
    }

    #[unsafe(no_mangle)]
    #[allow(non_upper_case_globals)]
    pub static lemnos_package: PackageDescriptor = PackageDescriptor::new(register);
}
