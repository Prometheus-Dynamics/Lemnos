//! A device with a `bus` and an optional need the spec doesn't name: the optional need is absent
//! and the bus goes to the need it is for.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use embedded_hal::i2c::{self, Operation};
use lemnos_core::{Config, I2c, Lifecycle, Line, Result, contract, driver};
use lemnos_host_static::{Device, Lock, Options, Slot, Spec, State, Table};
use lemnos_mcu::{Buses, DynI2c, I2cBus, Stall, Stalled};

struct Locked<T>(Mutex<T>);

impl<T: Send> Lock for Locked<T> {
    type Inner = T;

    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.0.try_lock().ok().map(|mut value| f(&mut value))
    }
}

struct Steps(AtomicU64);

impl lemnos_core::HostClock for Steps {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A bus that answers every transaction.
struct Quiet;

impl i2c::ErrorType for Quiet {
    type Error = i2c::ErrorKind;
}

impl i2c::I2c for Quiet {
    fn transaction(
        &mut self,
        _: u8,
        _: &mut [Operation<'_>],
    ) -> core::result::Result<(), Self::Error> {
        Ok(())
    }
}

#[contract("test-alarm", version = 1)]
pub trait Alarm {
    #[method(1)]
    fn has_line(&mut self) -> Result<bool>;
}

/// An optional line declared before the bus it really needs.
#[driver("test-alarm", provides = [Alarm])]
pub struct WithAlarm {
    #[needs(optional)]
    alert: Option<Line>,
    #[needs]
    bus: I2c,
}

impl Lifecycle for WithAlarm {
    type Needs = WithAlarmNeeds;

    fn start(needs: WithAlarmNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            alert: needs.alert,
            bus: needs.bus,
        })
    }
}

impl Alarm for WithAlarm {
    fn has_line(&mut self) -> Result<bool> {
        let _ = &mut self.bus;
        Ok(self.alert.is_some())
    }
}

#[test]
fn an_optional_need_the_spec_does_not_name_is_absent_and_the_bus_binds() {
    static CLOCK: Steps = Steps(AtomicU64::new(0));
    static SLOT: Slot<WithAlarm, Locked<Device<WithAlarm>>> = Slot::new(
        Spec::new("alarm").bus("i2c0"),
        Locked(Mutex::new(Device::new())),
    );
    static TABLE: Table = Table::new(&[&SLOT], &CLOCK, Options::DEFAULT);
    static STALL: Stall<DynI2c, Locked<Option<&'static mut DynI2c>>> =
        Stall::new("i2c0", Locked(Mutex::new(None)));
    assert!(STALL.put(Box::leak(Box::new(I2cBus(Quiet)))));
    let i2c = [&STALL as &'static dyn Stalled<DynI2c>];

    TABLE.start(&mut Buses::new().with_i2c(&i2c));
    assert_eq!(TABLE.state(0), Some(State::Running));
    assert!(!STALL.is_in(), "the driver holds the bus");
}
