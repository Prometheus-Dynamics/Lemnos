//! A driver that declares a `HostClock` need on a static table reads the table's own clock, with
//! no board entry naming it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use lemnos_core::{Config, HostClockHandle, Lifecycle, MethodId, Result, contract, driver};
use lemnos_host_static::{Device, Lock, Options, Slot, Spec, State, Table};
use lemnos_mcu::Buses;

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

#[contract("test-clock-read", version = 1)]
pub trait ClockRead {
    #[method(1)]
    fn read_ns(&mut self) -> Result<u64>;
}

#[driver("test-clock-probe", provides = [ClockRead])]
pub struct ClockProbe {
    #[needs]
    clock: HostClockHandle,
}

impl Lifecycle for ClockProbe {
    type Needs = ClockProbeNeeds;

    fn start(needs: ClockProbeNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self { clock: needs.clock })
    }
}

impl ClockRead for ClockProbe {
    fn read_ns(&mut self) -> Result<u64> {
        Ok(self.clock.now_ns())
    }
}

#[test]
fn a_driver_that_declares_the_clock_reads_the_tables_clock() {
    static CLOCK: Steps = Steps(AtomicU64::new(5));
    static SLOT: Slot<ClockProbe, Locked<Device<ClockProbe>>> =
        Slot::new(Spec::new("probe"), Locked(Mutex::new(Device::new())));
    static TABLE: Table = Table::new(&[&SLOT], &CLOCK, Options::DEFAULT);

    TABLE.start(&mut Buses::new());
    assert_eq!(TABLE.state(0), Some(State::Running));
    let method = MethodId {
        contract: 0,
        method: 1,
    };
    let mut reply = [0; 8];
    CLOCK.0.store(1_234_000, Ordering::Relaxed);
    assert_eq!(TABLE.call(0, method, &[], &mut reply).unwrap(), 8);
    assert_eq!(u64::from_le_bytes(reply), 1_234_000);
    CLOCK.0.store(9_000_000, Ordering::Relaxed);
    TABLE.call(0, method, &[], &mut reply).unwrap();
    assert_eq!(u64::from_le_bytes(reply), 9_000_000);
}
