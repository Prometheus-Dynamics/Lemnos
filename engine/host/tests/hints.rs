//! A backend's hints that hardware came or went make the host act now, not when a wait ends.
//! They are only hints: everything still happens without them, and a storm of them binds
//! nothing more often than the gap allows.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{
    Config, ConfigError, Dev, ErrorKind, Health, Hint, Key, Kind, Lifecycle, Line, Result,
    contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

const PORT: &str = "port-a";

static TICKS: AtomicU32 = AtomicU32::new(0);

#[contract("plugged", version = 1)]
pub trait Plugged {
    #[method(1)]
    fn answer(&mut self) -> Result<u32>;
}

const DEAD: &str = "what it held is dead";

/// Needs something that can be absent, the way a board file names a USB port, and looks at it
/// on every tick.
#[driver("plug", provides = [Plugged])]
pub struct Plug {
    #[needs]
    wire: Line,
    counts_ticks: bool,
    dead: bool,
}

impl Lifecycle for Plug {
    type Needs = PlugNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("counts_ticks", Kind::Bool)])
    }
    const TICKS: bool = true;

    fn start(needs: PlugNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            wire: needs.wire,
            counts_ticks: cfg.bool("counts_ticks").unwrap_or(false),
            dead: false,
        })
    }

    fn health(&self) -> Health {
        if self.dead {
            Health::Gone(DEAD)
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        if self.counts_ticks {
            TICKS.fetch_add(1, Ordering::Relaxed);
        }
        self.dead = self.wire.get() == Err(ErrorKind::Unavailable);
        None
    }
}

impl Plugged for Plug {
    fn answer(&mut self) -> Result<u32> {
        Ok(42)
    }
}

/// Needs a `plug` device, so it can bind only once that has.
#[driver("user", provides = [])]
pub struct User {
    #[needs]
    _part: Dev<dyn Plugged>,
}

impl Lifecycle for User {
    type Needs = UserNeeds;

    fn start(needs: UserNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self { _part: needs._part })
    }
}

const ONE: &str = "[[devices]]\nid = \"cam\"\ndriver = \"plug\"\nbus = \"port-a\"\n";

struct Rig {
    host: Host,
    clock: SimClock,
    board: SimBoard,
}

impl Rig {
    fn start(devices: &str) -> Self {
        let text = format!(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}"
        );
        let board = SimBoard::new();
        let clock = board.clock();
        clock.advance(Duration::from_secs(100));
        let host = Host::start(
            &parse(&text).unwrap(),
            &[entry::<Plug>(), entry::<User>()],
            Box::new(board.clone()),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        Self { host, clock, board }
    }

    /// Pulls the part out: whoever holds it finds it dead from now on.
    fn unplug(&self, name: &str) {
        let faults = self.board.faults(name).unwrap();
        let now = Duration::from_nanos(self.clock.now_ns());
        faults.outage(now, None, ErrorKind::Unavailable);
        self.board.remove(name);
    }

    /// Hands the host what the board has said since last time.
    fn deliver(&self) -> usize {
        self.board.hints().drain(|hint| self.host.hint(hint))
    }

    fn state(&self, id: &str) -> DeviceState {
        self.host.device(id).unwrap().state
    }

    /// Binds tried and failed in a row, and how long until the next.
    fn waiting(&self, id: &str) -> (u32, Duration) {
        match self.state(id) {
            DeviceState::Backoff {
                attempts,
                next_retry_ns,
                ..
            } => (
                attempts,
                Duration::from_nanos(next_retry_ns.saturating_sub(self.clock.now_ns())),
            ),
            other => panic!("{id} is not waiting: {other:?}"),
        }
    }

    /// Lets retries run until the device waits the longest it ever does, and a second of that
    /// wait go by.
    fn wait_out_the_ladder(&self, id: &str) {
        while self.waiting(id).1 < Options::default().retry_cap {
            self.clock.advance(self.waiting(id).1);
            self.host.tick();
        }
        self.clock.advance(Duration::from_secs(1));
        self.host.tick();
    }
}

#[test]
fn a_device_absent_at_start_binds_as_soon_as_a_hint_says_it_arrived() {
    let rig = Rig::start(ONE);
    rig.wait_out_the_ladder("cam");
    assert_eq!(rig.waiting("cam").1, Duration::from_secs(7));

    rig.board.add_line(PORT);
    rig.host.tick();
    assert_eq!(rig.waiting("cam").1, Duration::from_secs(7), "no hint yet");

    assert_eq!(rig.deliver(), 1);
    assert_eq!(rig.waiting("cam").1, Duration::ZERO, "due now, and visibly");
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
}

#[test]
fn a_hint_that_is_lost_costs_only_the_wait() {
    let rig = Rig::start(ONE);
    rig.wait_out_the_ladder("cam");
    rig.board.add_line(PORT);
    assert_eq!(rig.board.hints().lose(), 1);

    rig.clock
        .advance(Duration::from_secs(7) - Duration::from_nanos(1));
    rig.host.tick();
    assert!(matches!(rig.state("cam"), DeviceState::Backoff { .. }));
    rig.clock.advance(Duration::from_nanos(1));
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
}

#[test]
fn a_hint_for_another_name_changes_nothing() {
    let rig = Rig::start(ONE);
    rig.wait_out_the_ladder("cam");
    rig.board.add_line("port-b");
    assert_eq!(rig.deliver(), 1);
    assert_eq!(rig.waiting("cam").1, Duration::from_secs(7));
}

#[test]
fn a_storm_of_hints_binds_no_more_often_than_the_gap() {
    let rig = Rig::start(ONE);
    rig.wait_out_the_ladder("cam");
    let (before, _) = rig.waiting("cam");

    // Hints with no time passing: one bind, however many.
    for _ in 0..1000 {
        rig.host.hint(Hint::Changed(PORT));
        rig.host.hint(Hint::Any);
        rig.host.tick();
    }
    assert_eq!(rig.waiting("cam").0, before + 1);

    // A thousand a second for two seconds: a bind every 250 ms and no more.
    for _ in 0..2000 {
        rig.clock.advance(Duration::from_millis(1));
        rig.host.hint(Hint::Changed(PORT));
        rig.host.tick();
    }
    assert_eq!(rig.waiting("cam").0, before + 1 + 8);

    // The storm over, the wait is the failure rule's again, and it was never made longer.
    let (_, wait) = rig.waiting("cam");
    assert!(wait <= Duration::from_secs(8), "{wait:?}");
    rig.clock.advance(Duration::from_secs(8));
    rig.host.tick();
    assert_eq!(
        rig.waiting("cam"),
        (before + 1 + 8 + 1, Duration::from_secs(8))
    );
}

#[test]
fn a_hint_never_puts_a_bind_off() {
    let rig = Rig::start(ONE);
    // Just tried, due in 1 s. The gap would allow one in 250 ms, so that is when.
    assert_eq!(rig.waiting("cam"), (1, Duration::from_secs(1)));
    rig.host.hint(Hint::Changed(PORT));
    assert_eq!(rig.waiting("cam"), (1, Duration::from_millis(250)));
    // Due sooner than the gap would allow: left alone.
    rig.clock.advance(Duration::from_millis(200));
    rig.host.hint(Hint::Changed(PORT));
    assert_eq!(rig.waiting("cam"), (1, Duration::from_millis(50)));
}

#[test]
fn hardware_that_leaves_takes_its_driver_with_it_and_brings_it_back_when_it_returns() {
    let rig = Rig::start(ONE);
    rig.board.add_line(PORT);
    rig.deliver();
    rig.clock.advance(Duration::from_secs(1));
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
    assert!(rig.board.is_claimed(PORT));
    rig.clock.advance(Duration::from_secs(60));

    // Pulled out, with nobody calling the device and the driver asking for no tick.
    rig.unplug(PORT);
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running, "nothing has looked");
    assert_eq!(rig.deliver(), 1);
    rig.host.tick();
    assert!(matches!(
        rig.state("cam"),
        DeviceState::Backoff { ref reason, .. } if reason == DEAD
    ));
    assert!(!rig.board.is_claimed(PORT), "the dead handle is let go");
    rig.clock.advance(Duration::from_secs(1));
    rig.host.tick();
    assert!(matches!(rig.state("cam"), DeviceState::Backoff { .. }));

    // Back in: bound at once.
    rig.clock.advance(Duration::from_millis(300));
    rig.board.add_line(PORT);
    assert_eq!(rig.deliver(), 1);
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
    assert!(rig.board.is_claimed(PORT));
}

#[test]
fn pulled_out_and_put_back_before_anyone_looked_is_one_new_bind() {
    let rig = Rig::start(ONE);
    rig.board.add_line(PORT);
    rig.deliver();
    rig.clock.advance(Duration::from_secs(1));
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
    rig.clock.advance(Duration::from_secs(60));

    rig.unplug(PORT);
    rig.board.add_line(PORT);
    assert_eq!(rig.deliver(), 2);
    // The driver finds its handle dead, and the bind that follows doesn't wait a second.
    rig.host.tick();
    assert_eq!(rig.waiting("cam"), (1, Duration::ZERO));
    assert!(!rig.board.is_claimed(PORT));
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
    assert!(rig.board.is_claimed(PORT));
}

#[test]
fn a_device_waiting_on_another_is_tried_as_soon_as_that_one_binds() {
    // The user comes first in the file, so it is looked at before the device it needs.
    let rig = Rig::start(&format!(
        "[[devices]]\nid = \"top\"\ndriver = \"user\"\nuses = {{ _part = \"cam\" }}\n{ONE}"
    ));
    rig.wait_out_the_ladder("cam");
    rig.wait_out_the_ladder("top");

    rig.board.add_line(PORT);
    rig.deliver();
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
    assert_eq!(rig.waiting("top").1, Duration::ZERO, "brought forward");
    rig.host.tick();
    assert_eq!(rig.state("top"), DeviceState::Running);
}

#[test]
fn when_the_backend_cannot_say_what_changed_everything_waiting_is_tried() {
    let rig = Rig::start(&format!(
        "{ONE}[[devices]]\nid = \"other\"\ndriver = \"plug\"\nbus = \"port-b\"\n"
    ));
    rig.wait_out_the_ladder("cam");
    rig.wait_out_the_ladder("other");
    rig.board.add_line(PORT);
    rig.board.add_line("port-b");
    rig.board.hints().lose();

    rig.host.hint(Hint::Any);
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
    assert_eq!(rig.state("other"), DeviceState::Running);
}

#[test]
fn a_running_device_that_ticks_is_ticked_when_its_name_is_hinted_and_wakes_the_loop() {
    let rig = Rig::start(
        "[[devices]]\nid = \"cam\"\ndriver = \"plug\"\nbus = \"port-a\"\n\
         config = { counts_ticks = true }\n",
    );
    rig.board.add_line(PORT);
    rig.deliver();
    rig.clock.advance(Duration::from_secs(1));
    rig.host.tick();
    assert_eq!(rig.state("cam"), DeviceState::Running);
    let before = TICKS.load(Ordering::Relaxed);
    rig.host.tick();
    assert_eq!(
        TICKS.load(Ordering::Relaxed),
        before,
        "it asked for no tick"
    );

    let wakes = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&wakes);
    rig.host.set_waker(move || {
        seen.fetch_add(1, Ordering::Relaxed);
    });
    rig.host.hint(Hint::Changed(PORT));
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    rig.host.tick();
    assert_eq!(TICKS.load(Ordering::Relaxed), before + 1);
}

#[test]
fn a_hint_wakes_whoever_runs_the_host_only_when_there_is_something_to_do() {
    let rig = Rig::start(ONE);
    rig.wait_out_the_ladder("cam");
    let wakes = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&wakes);
    rig.host.set_waker(move || {
        seen.fetch_add(1, Ordering::Relaxed);
    });
    rig.host.hint(Hint::Changed("port-b"));
    assert_eq!(wakes.load(Ordering::Relaxed), 0);
    rig.host.hint(Hint::Changed(PORT));
    rig.host.hint(Hint::Changed(PORT));
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        1,
        "the second changed nothing"
    );
}
