//! The host stamps the moment a device first runs with health Ok in a bind: the moment it ran the
//! driver and saw that, on its clock, not the moment anyone looks. A rebind starts a fresh stamp,
//! and a device that goes Ok, degraded and Ok again within one bind keeps its first.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{
    Config, ConfigError, Health, Key, Kind, Lifecycle, MethodId, Result, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{Host, entry};
use lemnos_sim::{SimBoard, SimClock};

mod common;
mod test_options;
use common::{MAG, SIX_AXIS};

/// The clock starts here, so a bind is not at zero.
const START: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;

#[contract("warmer", version = 1)]
pub trait Warming {
    /// 0 is ok, 1 is degraded.
    #[method(2)]
    fn set_mode(&mut self, mode: u32) -> Result<()>;
}

/// Not up until a tick finds its time has come. Degraded while its mode says so.
#[driver("warmer", provides = [Warming])]
pub struct Warmer {
    ready_at: u64,
    up: bool,
    mode: u32,
}

impl Lifecycle for Warmer {
    type Needs = WarmerNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("ready_at_ns", Kind::Int)])
    }
    const TICKS: bool = true;

    fn start(_: WarmerNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            ready_at: cfg.int("ready_at_ns").unwrap_or(0) as u64,
            up: false,
            mode: 0,
        })
    }

    fn health(&self) -> Health {
        if !self.up {
            Health::Failed("warming up")
        } else if self.mode == 1 {
            Health::Degraded("flaky")
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if now_ns >= self.ready_at {
            self.up = true;
            None
        } else {
            Some(self.ready_at)
        }
    }
}

impl Warming for Warmer {
    fn set_mode(&mut self, mode: u32) -> Result<()> {
        self.mode = mode;
        Ok(())
    }
}

fn host(ready_at_ns: u64) -> (Host, SimClock) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"w\"\ndriver = \"warmer\"\nconfig = {{ ready_at_ns = {ready_at_ns} }}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    clock.advance(Duration::from_nanos(START));
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Warmer>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        test_options::options(),
    )
    .unwrap();
    (host, clock)
}

fn method(id: u16) -> MethodId {
    MethodId {
        contract: 0,
        method: id,
    }
}

#[test]
fn the_stamp_is_when_the_driver_came_up_not_when_anyone_looked() {
    let bring_up = 40 * MS;
    let (host, clock) = host(START + bring_up);
    let index = host.device("w").unwrap().index;
    assert_eq!(host.bound_ns(index), Some(START));
    assert_eq!(host.running_ok_ns(index), None, "not up yet");

    clock.advance(Duration::from_nanos(bring_up));
    host.run_ticks();
    // The look comes much later, as the daemon's does, and finds the device ok.
    clock.advance(Duration::from_secs(5));
    assert_eq!(host.look(index).map(|(_, health)| health), Some(Health::Ok));

    let ok = host.running_ok_ns(index).unwrap();
    assert_eq!(ok, START + bring_up, "the moment the driver came up");
    assert_eq!(ok - host.bound_ns(index).unwrap(), bring_up);
}

#[test]
fn a_rebind_gets_a_fresh_stamp() {
    let (host, clock) = host(0);
    let index = host.device("w").unwrap().index;
    clock.advance(Duration::from_nanos(10 * MS));
    host.run_ticks();
    let first = host.running_ok_ns(index).unwrap();
    assert_eq!(first, START + 10 * MS);

    clock.advance(Duration::from_secs(2));
    host.off("w").unwrap();
    host.on("w").unwrap();
    assert_eq!(
        host.running_ok_ns(index),
        None,
        "a new bind starts with no stamp"
    );

    clock.advance(Duration::from_nanos(5 * MS));
    host.run_ticks();
    let second = host.running_ok_ns(index).unwrap();
    assert_ne!(second, first);
    assert_eq!(host.bound_ns(index), Some(START + 10 * MS + 2_000 * MS));
    assert_eq!(second - host.bound_ns(index).unwrap(), 5 * MS);
}

#[test]
fn ok_then_degraded_then_ok_within_one_bind_keeps_the_first_stamp() {
    let (host, clock) = host(0);
    let index = host.device("w").unwrap().index;
    clock.advance(Duration::from_nanos(10 * MS));
    host.run_ticks();
    let first = host.running_ok_ns(index).unwrap();
    assert_eq!(first, START + 10 * MS);

    clock.advance(Duration::from_nanos(20 * MS));
    host.call(index, method(2), &1u32.to_le_bytes(), &mut [])
        .unwrap();
    assert!(matches!(host.look(index), Some((_, Health::Degraded(_)))));

    clock.advance(Duration::from_nanos(30 * MS));
    host.call(index, method(2), &0u32.to_le_bytes(), &mut [])
        .unwrap();
    assert_eq!(host.look(index).map(|(_, health)| health), Some(Health::Ok));
    assert_eq!(host.running_ok_ns(index), Some(first));
}

/// A device whose Ok depends on its children (the orientation device needs three sensors) has no
/// stamp from a driver run of its own. The first look that finds it running ok stamps it, so its
/// stamp is that look's time, and a later look does not move it.
#[test]
fn a_device_whose_ok_depends_on_its_children_is_stamped_by_the_first_look_that_finds_it_ok() {
    let (host, clock) = common::host(&format!("{SIX_AXIS}{MAG}"));
    let index = host.device("orientation").unwrap().index;
    let bound = host.bound_ns(index).unwrap();
    assert_eq!(host.running_ok_ns(index), None, "no look has found it yet");

    clock.advance(Duration::from_nanos(7 * MS));
    assert_eq!(host.look(index).map(|(_, health)| health), Some(Health::Ok));
    let stamp = host.running_ok_ns(index).unwrap();
    assert_eq!(stamp, bound + 7 * MS, "the look's time, not the bind's");

    clock.advance(Duration::from_nanos(50 * MS));
    assert_eq!(host.look(index).map(|(_, health)| health), Some(Health::Ok));
    assert_eq!(
        host.running_ok_ns(index),
        Some(stamp),
        "the first look wins"
    );
}
