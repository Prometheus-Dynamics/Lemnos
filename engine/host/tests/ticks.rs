//! A driver that asks for time gets it: when it said, and not otherwise.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{Config, ConfigError, Key, Kind, Lifecycle, MethodId, Result, contract, driver};
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

#[contract("ticker", version = 1)]
pub trait Ticking {
    #[method(1)]
    fn fired(&mut self) -> Result<u32>;
    #[method(2)]
    fn arm(&mut self, delay_ns: u64) -> Result<()>;
    #[method(3)]
    fn panic_next(&mut self) -> Result<()>;
}

#[driver("ticker", provides = [Ticking])]
pub struct Ticker {
    due: u64,
    period: u64,
    arm: Option<u64>,
    fired: u32,
    panic_next: bool,
}

impl Lifecycle for Ticker {
    type Needs = TickerNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("period_ns", Kind::Int)])
    }
    const TICKS: bool = true;

    fn start(_: TickerNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            due: 0,
            period: cfg.int("period_ns").unwrap_or(0) as u64,
            arm: None,
            fired: 0,
            panic_next: false,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        assert!(!self.panic_next, "asked to");
        if let Some(delay) = self.arm.take() {
            self.due = now_ns + delay;
        }
        if now_ns >= self.due {
            self.fired += 1;
            self.due = if self.period > 0 {
                now_ns + self.period
            } else {
                u64::MAX
            };
        }
        (self.due != u64::MAX).then_some(self.due)
    }
}

impl Ticking for Ticker {
    fn fired(&mut self) -> Result<u32> {
        Ok(self.fired)
    }

    fn arm(&mut self, delay_ns: u64) -> Result<()> {
        self.arm = Some(delay_ns);
        Ok(())
    }

    fn panic_next(&mut self) -> Result<()> {
        self.panic_next = true;
        Ok(())
    }
}

fn host(period_ns: u64) -> (Host, SimClock) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"t\"\ndriver = \"ticker\"\nconfig = {{ period_ns = {period_ns} }}\n\
         [[devices]]\nid = \"c\"\ndriver = \"fake-counter\"\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Ticker>(), entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

fn fired(host: &Host) -> u32 {
    let index = host.device("t").unwrap().index;
    let mut reply = [0; 4];
    host.call(index, method(1), &[], &mut reply).unwrap();
    u32::from_le_bytes(reply)
}

#[test]
fn a_driver_is_ticked_once_it_starts_and_then_when_it_asked() {
    let (host, clock) = host(1_000);
    assert_eq!(
        host.next_tick_ns(),
        Some(0),
        "due as soon as it has started"
    );
    host.run_ticks();
    assert_eq!(fired(&host), 1);

    clock.advance(Duration::from_nanos(999));
    host.run_ticks();
    assert_eq!(fired(&host), 1, "not before its time");

    clock.advance(Duration::from_nanos(1));
    assert_eq!(host.next_tick_ns(), Some(1_000));
    host.run_ticks();
    assert_eq!(fired(&host), 2);
    assert_eq!(host.next_tick_ns(), Some(2_000));
}

#[test]
fn a_host_tick_runs_ticks_too() {
    let (host, _clock) = host(1_000);
    host.tick();
    assert_eq!(fired(&host), 1);
}

#[test]
fn a_driver_that_wants_nothing_is_left_alone_until_a_method_starts_it() {
    let (host, clock) = host(0);
    host.run_ticks();
    assert_eq!(fired(&host), 1);
    assert_eq!(host.next_tick_ns(), None);

    let wakes = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&wakes);
    host.set_waker(move || {
        seen.fetch_add(1, Ordering::Relaxed);
    });

    clock.advance(Duration::from_nanos(50));
    let index = host.device("t").unwrap().index;
    host.call(index, method(2), &500u64.to_le_bytes(), &mut [])
        .unwrap();
    assert_eq!(host.next_tick_ns(), Some(550), "the call scheduled it");
    assert_eq!(wakes.load(Ordering::Relaxed), 1, "and woke whoever sleeps");

    clock.advance(Duration::from_nanos(500));
    host.run_ticks();
    assert_eq!(fired(&host), 2);
    assert_eq!(host.next_tick_ns(), None);
}

#[test]
fn a_panic_in_a_tick_is_contained_and_the_rebuilt_driver_ticks_again() {
    let (host, clock) = host(1_000);
    host.run_ticks();
    let index = host.device("t").unwrap().index;
    // The tick after this call is the one that panics.
    let refused = host.call(index, method(3), &[], &mut []);
    assert!(refused.is_err(), "{refused:?}");
    assert!(matches!(
        host.device("t").unwrap().state,
        DeviceState::Backoff { .. }
    ));
    assert_eq!(host.next_tick_ns(), None, "nothing due while it is down");

    clock.advance(Duration::from_secs(60));
    host.tick();
    assert_eq!(host.device("t").unwrap().state, DeviceState::Running);
    assert_eq!(fired(&host), 1, "a new instance, ticked once");
}
