//! A whole driver package: a contract, a `#[driver]` type that serves it, and the one line that
//! exports it. The driver is written exactly as a built-in one is.
//!
//! It has no hardware behind it. Its methods are what the loader's tests need: a value that
//! comes back right, a panic, a hang, a call to another device and a read from a bus.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{
    Config, Description, Dev, Health, I2c, Item, Lifecycle, MethodId, Result, Status, contract,
    driver, status_of,
};
use lemnos_hal::i2c::I2c as _;
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
}

/// Binds that have panicked, in this package.
static PANICS: AtomicU32 = AtomicU32::new(0);

#[driver("package-fixture", provides = [Example])]
pub struct Fixture {
    #[needs(optional)]
    peer: Option<Dev<dyn Example>>,
    #[needs(optional)]
    bus: Option<I2c>,
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
