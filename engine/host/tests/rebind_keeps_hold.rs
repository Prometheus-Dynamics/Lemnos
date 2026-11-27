//! A rebind that keeps the endpoints' hold: a driver that parks what the holder of its endpoints
//! uses ([`Lifecycle::rebinding`]) is kept until the new instance's first tick has run, END stays
//! open through it, and the new instance is lent the holder's pair. A bind that fails, a
//! switch-off and a shutdown let the kept instance go and end the hold; a parked session nothing
//! took up is taken down with it, never left behind.

mod test_options;

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lemnos_core::{
    BufferHandle, CallerId, Config, ConfigError, Health, Key, Kind, Lifecycle, MethodId, Result,
    Status, contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

const GONE: &str = "what it was bound with went";
const CHANGED: &str = "the function changed";

/// The host's warnings, as the daemon's journal would have them. Tests share the process and
/// every device is `d`: each test looks for the reason only it gives.
static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn listen() {
    lemnos_host::set_warning_sink(|line| HEARD.lock().unwrap().push(line.to_string()));
}

/// Whether the host logged a line about device `d` that says `text`.
fn heard(text: &str) -> bool {
    HEARD
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.starts_with("device d (driver lender): ") && line.contains(text))
}

/// What one test's instances share, picked by the device's `key`: tests share the process.
struct Shared {
    /// Each instance's number, from 1, in the order they were bound.
    binds: AtomicU32,
    /// The next tick finds its resources gone.
    die: AtomicBool,
    /// Whether an instance parks for the next one when asked.
    parks: AtomicBool,
    /// The next bind is refused.
    refuse: AtomicBool,
    /// The next instance finds the parked part was another (a changed function): it does not
    /// take it up.
    changed: AtomicBool,
    /// The park waiting to be taken up: the number of the instance that parked.
    parked: Mutex<Option<u32>>,
    /// What happened, in order: `parked N`, `took up N`, `dropped N`, `lent N to C`,
    /// `taken down N` (a parked session nothing took up, taken down by its instance's drop).
    log: Mutex<Vec<String>>,
}

const KEYS: usize = 12;
static SHARED: [Shared; KEYS] = [const {
    Shared {
        binds: AtomicU32::new(0),
        die: AtomicBool::new(false),
        parks: AtomicBool::new(true),
        refuse: AtomicBool::new(false),
        changed: AtomicBool::new(false),
        parked: Mutex::new(None),
        log: Mutex::new(Vec::new()),
    }
}; KEYS];

impl Shared {
    fn note(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }

    fn log(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

#[contract("hang", version = 1)]
pub trait Hang {
    /// Runs for `ms`, then finds its resources gone if `gone` is not 0.
    #[method(1)]
    fn hang(&mut self, ms: u32) -> Result<u32>;
}

#[driver("lender", provides = [Hang])]
pub struct Lender {
    shared: &'static Shared,
    number: u32,
    gone: bool,
    holder: Option<CallerId>,
    /// The pair it lends: the same two files for every instance, as a parked session's are.
    pair: Arc<[OwnedFd; 2]>,
    /// This instance parked, and its park is its own until taken up.
    parked: bool,
    /// This instance took up a park at its first tick.
    took_up: bool,
    started: bool,
}

static PAIRS: [Mutex<Option<Arc<[OwnedFd; 2]>>>; KEYS] = [const { Mutex::new(None) }; KEYS];

fn pair(key: usize) -> Arc<[OwnedFd; 2]> {
    PAIRS[key]
        .lock()
        .unwrap()
        .get_or_insert_with(|| {
            let (a, b) = std::io::pipe().unwrap();
            Arc::new([OwnedFd::from(a), OwnedFd::from(b)])
        })
        .clone()
}

impl Lifecycle for Lender {
    type Needs = LenderNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("key", Kind::Int)])
    }
    const TICKS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn start(_: LenderNeeds, cfg: &Config<'_>) -> Result<Self> {
        let key = cfg.int("key").ok_or(Status::INVALID_ARGUMENT)? as usize;
        let shared = &SHARED[key];
        if shared.refuse.load(Ordering::Relaxed) {
            return Err(Status::IO);
        }
        let number = shared.binds.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(Self {
            shared,
            number,
            gone: false,
            holder: None,
            pair: pair(key),
            parked: false,
            took_up: false,
            started: false,
        })
    }

    fn health(&self) -> Health {
        if self.gone {
            Health::Gone(GONE)
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if !self.started {
            self.started = true;
            // The first tick takes up a parked session, as an enable does.
            if let Some(from) = self.shared.parked.lock().unwrap().take() {
                if self.shared.changed.swap(false, Ordering::Relaxed) {
                    self.shared.note(format!("replaced {from}"));
                } else {
                    self.took_up = true;
                    self.shared.note(format!("took up {from}"));
                }
            }
        }
        if self.shared.die.swap(false, Ordering::Relaxed) {
            self.gone = true;
        }
        Some(now_ns + 1_000_000)
    }

    fn rebinding(&mut self) -> bool {
        if !self.shared.parks.load(Ordering::Relaxed) {
            return false;
        }
        *self.shared.parked.lock().unwrap() = Some(self.number);
        self.parked = true;
        self.shared.note(format!("parked {}", self.number));
        true
    }

    fn took_up_link(&self) -> core::result::Result<(), &'static str> {
        if self.took_up { Ok(()) } else { Err(CHANGED) }
    }

    fn endpoints(&mut self, caller: CallerId) -> Result<[BufferHandle<'_>; 2]> {
        if self.holder.is_some_and(|holder| holder != caller) {
            return Err(Status::BUSY);
        }
        self.holder = Some(caller);
        self.shared
            .note(format!("lent {} to {}", self.number, caller.0));
        Ok([
            BufferHandle::from(self.pair[0].as_fd()),
            BufferHandle::from(self.pair[1].as_fd()),
        ])
    }

    fn release_endpoints(&mut self, caller: CallerId) -> Result<()> {
        if self.holder != Some(caller) {
            return Err(Status::NOT_FOUND);
        }
        self.holder = None;
        Ok(())
    }

    fn caller_gone(&mut self, caller: CallerId) {
        if self.holder == Some(caller) {
            self.holder = None;
        }
    }
}

impl Drop for Lender {
    fn drop(&mut self) {
        if self.parked {
            let mut parked = self.shared.parked.lock().unwrap();
            if *parked == Some(self.number) {
                *parked = None;
                drop(parked);
                self.shared.note(format!("taken down {}", self.number));
            }
        }
        self.shared.note(format!("dropped {}", self.number));
    }
}

impl Hang for Lender {
    fn hang(&mut self, ms: u32) -> Result<u32> {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
        Ok(ms)
    }
}

fn host_with(key: usize, options: Options) -> (Host, SimClock, &'static Shared) {
    listen();
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"d\"\ndriver = \"lender\"\nconfig = {{ key = {key} }}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Lender>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        options,
    )
    .unwrap();
    let shared = &SHARED[key];
    host.tick();
    shared.log();
    (host, clock, shared)
}

fn host(key: usize) -> (Host, SimClock, &'static Shared) {
    host_with(key, test_options::options())
}

const HOLDER: CallerId = CallerId(7);

fn index(host: &Host) -> u32 {
    host.device("d").unwrap().index
}

/// Whether END reads EOF or hangs up: the hold is over.
fn over(end: &OwnedFd) -> bool {
    let mut fds = [rustix::event::PollFd::new(
        end,
        rustix::event::PollFlags::IN,
    )];
    let now = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    rustix::event::poll(&mut fds, Some(&now)).is_ok_and(|n| n > 0)
}

/// The holder takes the pair with END, then the device finds its resources gone.
fn held_then_gone(host: &Host, clock: &SimClock, shared: &Shared) -> OwnedFd {
    let [_out, _input, end] = host.usb_endpoints_end(HOLDER, index(host)).unwrap();
    shared.die.store(true, Ordering::Relaxed);
    clock.advance(Duration::from_millis(1));
    host.tick();
    end
}

fn state(host: &Host) -> DeviceState {
    host.device("d").unwrap().state
}

#[test]
fn a_rebind_that_parks_keeps_the_hold_and_lends_the_new_instance_the_holders_pair() {
    let (host, clock, shared) = host(0);
    let end = held_then_gone(&host, &clock, shared);
    assert!(matches!(state(&host), DeviceState::Backoff { .. }));
    assert_eq!(
        shared.log(),
        ["lent 1 to 7", "parked 1"],
        "kept, not dropped"
    );
    assert!(!over(&end), "the hold goes on");

    // The new instance binds, and its first tick takes up the park: only then does the kept
    // instance go, and the holder's pair is lent to the new one.
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(shared.log(), ["took up 1", "dropped 1", "lent 2 to 7"]);
    assert!(!over(&end), "the hold goes on");
    // The new instance knows its holder: another caller is refused.
    assert_eq!(
        host.usb_endpoints(CallerId(8), index(&host)).err(),
        Some(Status::BUSY)
    );
    // The holder releases: END ends then.
    host.usb_endpoints_release(HOLDER, index(&host)).unwrap();
    assert!(over(&end));
}

#[test]
fn a_rebind_with_nobody_holding_the_pair_parks_nothing() {
    let (host, clock, shared) = host(1);
    shared.die.store(true, Ordering::Relaxed);
    clock.advance(Duration::from_millis(1));
    host.tick();
    assert_eq!(shared.log(), ["dropped 1"], "never asked: nothing to keep");
}

#[test]
fn a_driver_that_parks_nothing_ends_the_hold_at_the_bind() {
    let (host, clock, shared) = host(2);
    shared.parks.store(false, Ordering::Relaxed);
    let end = held_then_gone(&host, &clock, shared);
    assert_eq!(shared.log(), ["lent 1 to 7", "dropped 1"]);
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert!(over(&end), "the hold ends at the bind");
}

#[test]
fn two_rebinds_in_a_row_park_the_newest_instance_and_never_the_kept_one() {
    let (host, clock, shared) = host(3);
    let end = held_then_gone(&host, &clock, shared);
    assert_eq!(shared.log(), ["lent 1 to 7", "parked 1"]);
    // The new instance is gone at its first tick: its first tick takes up the park, the kept
    // instance goes, and the new one is what the second rebind parks.
    shared.die.store(true, Ordering::Relaxed);
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(
        shared.log(),
        ["took up 1", "dropped 1", "lent 2 to 7", "parked 2"]
    );
    assert!(!over(&end));
    clock.advance(Duration::from_secs(2));
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(shared.log(), ["took up 2", "dropped 2", "lent 3 to 7"]);
    assert!(!over(&end), "the hold went on through both");
}

#[test]
fn a_bind_that_fails_lets_the_kept_instance_go_and_its_park_is_taken_down() {
    let (host, clock, shared) = host(4);
    let end = held_then_gone(&host, &clock, shared);
    shared.log();
    shared.refuse.store(true, Ordering::Relaxed);
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert!(matches!(state(&host), DeviceState::Backoff { .. }));
    assert_eq!(
        shared.log(),
        ["taken down 1", "dropped 1"],
        "never orphaned"
    );
    assert!(over(&end), "the hold ends with it");
    assert!(heard(
        "the instance kept across the rebind is let go (the bind failed)"
    ));
    shared.refuse.store(false, Ordering::Relaxed);
}

#[test]
fn a_switch_off_while_an_instance_is_kept_lets_it_go_and_ends_the_hold() {
    let (host, clock, shared) = host(5);
    let end = held_then_gone(&host, &clock, shared);
    shared.log();
    host.off("d").unwrap();
    assert_eq!(shared.log(), ["taken down 1", "dropped 1"]);
    assert!(over(&end));
    assert!(heard(
        "the instance kept across the rebind is let go (the device is switched off)"
    ));
}

#[test]
fn a_shutdown_while_an_instance_is_kept_lets_it_go_and_ends_the_hold() {
    let (host, clock, shared) = host(8);
    let end = held_then_gone(&host, &clock, shared);
    shared.log();
    host.shutdown();
    assert_eq!(
        shared.log(),
        ["taken down 1", "dropped 1"],
        "never orphaned"
    );
    assert!(over(&end));
    assert!(heard(
        "the instance kept across the rebind is let go (the host shuts down)"
    ));
}

#[test]
fn a_new_instance_that_finds_the_function_changed_ends_the_hold_with_one_line() {
    let (host, clock, shared) = host(9);
    let end = held_then_gone(&host, &clock, shared);
    shared.log();
    shared.changed.store(true, Ordering::Relaxed);
    clock.advance(Duration::from_secs(1));
    host.tick();
    assert_eq!(state(&host), DeviceState::Running);
    assert_eq!(
        shared.log(),
        ["replaced 1", "dropped 1"],
        "the kept instance goes, and the holder's pair is not lent to the new one"
    );
    assert!(over(&end), "the hold ends");
    let lines: Vec<String> = HEARD
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains(CHANGED))
        .cloned()
        .collect();
    assert_eq!(
        lines,
        [
            "device d (driver lender): the link kept across the rebind is dropped: the function changed; the hold ends"
        ],
    );
    // The holder may take the new instance's pair as any caller does.
    host.usb_endpoints_release(HOLDER, index(&host)).ok();
    assert!(host.usb_endpoints_end(HOLDER, index(&host)).is_ok());
}

#[test]
fn a_switch_off_ends_the_hold_first_and_waits_a_moment_for_the_holder_to_let_go() {
    let (host, _clock, shared) = host(6);
    let [_out, _input, end] = host.usb_endpoints_end(HOLDER, index(&host)).unwrap();
    shared.log();
    // A holder that lets go once END says so: the switch-off goes on then, not after the wait.
    let host = Arc::new(host);
    let releaser = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            while !over(&end) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            host.usb_endpoints_release(HOLDER, index(&host)).unwrap();
        })
    };
    let started = Instant::now();
    host.off("d").unwrap();
    let took = started.elapsed();
    releaser.join().unwrap();
    assert!(took < Duration::from_millis(900), "{took:?}");
    assert_eq!(shared.log(), ["dropped 1"]);

    // A holder that never lets go: the switch-off waits at most a second.
    host.on("d").unwrap();
    host.tick();
    let [_out, _input, _end] = host.usb_endpoints_end(CallerId(9), index(&host)).unwrap();
    let started = Instant::now();
    host.off("d").unwrap();
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(950), "{took:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
}

/// A call into instance `n` overruns the deadline and returns: the bind after it keeps `n`,
/// and the next instance takes up its park and is lent the holder's pair.
fn overrun_and_rebind(host: &Host, clock: &SimClock, shared: &Shared, end: &OwnedFd, n: u32) {
    let method = MethodId {
        contract: 0,
        method: 1,
    };
    let mut reply = [0; 4];
    let ms = 400u32.to_le_bytes();
    assert!(host.call(index(host), method, &ms, &mut reply).is_err());
    // The call returns on its own; the bind after it finds the old instance free.
    std::thread::sleep(Duration::from_millis(500));
    let until = Instant::now() + Duration::from_secs(10);
    while state(host) != DeviceState::Running || shared.binds.load(Ordering::Relaxed) < n + 1 {
        assert!(Instant::now() < until, "no rebind: {:?}", state(host));
        clock.advance(Duration::from_secs(1));
        host.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    let log = shared.log();
    assert!(log.starts_with(&[format!("parked {n}")]), "{log:?}");
    assert!(!over(end), "the hold goes on: {log:?}");
    clock.advance(Duration::from_millis(2));
    host.tick();
    let log = [log, shared.log()].concat();
    assert!(
        log.ends_with(&[
            format!("took up {n}"),
            format!("dropped {n}"),
            format!("lent {} to 7", n + 1)
        ]),
        "{log:?}"
    );
    assert!(!over(end));
}

fn short_deadline() -> Options {
    Options {
        call_deadline: Duration::from_millis(100),
        ..Options::default()
    }
}

#[test]
fn a_call_that_overran_and_returned_is_kept_at_the_bind_when_it_parks() {
    let (host, clock, shared) = host_with(7, short_deadline());
    let [_out, _input, end] = host.usb_endpoints_end(HOLDER, index(&host)).unwrap();
    shared.log();
    overrun_and_rebind(&host, &clock, shared, &end, 1);
}

#[test]
fn two_overruns_and_rebinds_in_a_row_keep_the_hold() {
    let (host, clock, shared) = host_with(10, short_deadline());
    let [_out, _input, end] = host.usb_endpoints_end(HOLDER, index(&host)).unwrap();
    shared.log();
    overrun_and_rebind(&host, &clock, shared, &end, 1);
    overrun_and_rebind(&host, &clock, shared, &end, 2);
    assert!(!over(&end), "the hold went on through both");
}
