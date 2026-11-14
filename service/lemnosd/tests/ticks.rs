//! The real service's event loop gives a driver the time it asks for.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_core::{
    Config as DriverConfig, ConfigError, Key, Kind, Lifecycle, Result, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

/// Fires of the one-shot driver, readable without calling it.
static ONE_SHOTS: AtomicU32 = AtomicU32::new(0);

#[contract("ticker", version = 1)]
pub trait Ticking {
    #[method(1)]
    fn fired(&mut self) -> Result<u32>;
    #[method(2)]
    fn arm(&mut self, delay_ns: u64) -> Result<()>;
}

#[driver("ticker", provides = [Ticking])]
pub struct Ticker {
    due: u64,
    period: u64,
    arm: Option<u64>,
    fired: u32,
}

impl Lifecycle for Ticker {
    type Needs = TickerNeeds;

    fn check_config<'a>(cfg: &DriverConfig<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("period_ns", Kind::Int)])
    }
    const TICKS: bool = true;

    fn start(_: TickerNeeds, cfg: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {
            due: 0,
            period: cfg.int("period_ns").unwrap_or(0) as u64,
            arm: None,
            fired: 0,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if let Some(delay) = self.arm.take() {
            self.due = now_ns + delay;
        }
        if now_ns >= self.due {
            self.fired += 1;
            if self.period == 0 {
                ONE_SHOTS.fetch_add(1, Ordering::Release);
            }
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
}

fn start(period_ns: u64) -> (Service, std::path::PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnosd-ticks-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"t\"\ndriver = \"ticker\"\nconfig = {{ period_ns = {period_ns} }}\n"
    );
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<Ticker>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .unwrap();
    (service, socket)
}

#[test]
fn a_periodic_driver_keeps_being_ticked() {
    let (_service, socket) = start(2_000_000);
    let client = Client::connect(&socket, "test").unwrap();
    let mut ticker = client.open::<dyn Ticking>("t").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while ticker.fired().unwrap() < 5 {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The event loop sleeps up to 250 ms with nothing to do. A call that starts timed work has to
/// cut that short. The count is read outside the driver, because any call would tick it.
#[test]
fn a_call_that_starts_timed_work_wakes_the_sleeping_loop() {
    let (_service, socket) = start(0);
    let client = Client::connect(&socket, "test").unwrap();
    let mut ticker = client.open::<dyn Ticking>("t").unwrap();
    let mut took = Vec::new();
    for _ in 0..7 {
        // Long enough for the loop to be back asleep.
        std::thread::sleep(Duration::from_millis(30));
        let before = ONE_SHOTS.load(Ordering::Acquire);
        let start = Instant::now();
        ticker.arm(5_000_000).unwrap();
        let deadline = start + Duration::from_secs(20);
        while ONE_SHOTS.load(Ordering::Acquire) == before {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_micros(200));
        }
        took.push(start.elapsed());
    }
    took.sort();
    // The median, so one bad scheduling moment doesn't fail it. Without the wake the delay is
    // spread over the loop's whole 250 ms sleep.
    assert!(took[3] < Duration::from_millis(100), "took {took:?}");
}
