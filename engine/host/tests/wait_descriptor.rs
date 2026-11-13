//! A driver that names a descriptor to wait on is ticked when it wakes, and not otherwise: the
//! host waits on the descriptor in place of a tick deadline, and clears each wake.

use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lemnos_core::{BufferHandle, Config, Lifecycle, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

#[contract("waiter-counts", version = 1)]
pub trait Counts {
    #[method(1)]
    fn ticks(&mut self) -> Result<u32>;
}

/// The other end of the descriptor the driver waits on, handed to the test once it has started.
static PEER: OnceLock<Mutex<Option<UnixStream>>> = OnceLock::new();
/// Ticks the driver has had, for the test to read without a method call.
static TICKED: AtomicU64 = AtomicU64::new(0);

#[driver("waiter", provides = [Counts])]
pub struct Waiter {
    ticks: u32,
    wait: OwnedFd,
}

impl Lifecycle for Waiter {
    type Needs = WaiterNeeds;
    const TICKS: bool = true;

    fn start(_: WaiterNeeds, _cfg: &Config<'_>) -> Result<Self> {
        let (ours, peer) = UnixStream::pair().map_err(|_| Status::IO)?;
        *PEER.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(peer);
        Ok(Self {
            ticks: 0,
            wait: OwnedFd::from(ours),
        })
    }

    /// Asks for no deadline: only a wake of the descriptor ticks it again.
    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        self.ticks += 1;
        TICKED.fetch_add(1, Ordering::SeqCst);
        None
    }

    fn wait_fd(&self) -> Option<BufferHandle<'_>> {
        Some(BufferHandle::from(self.wait.as_fd()))
    }
}

impl Counts for Waiter {
    fn ticks(&mut self) -> Result<u32> {
        Ok(self.ticks)
    }
}

fn host() -> (Host, SimClock) {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"w\"\n\
                [[devices]]\nid = \"w\"\ndriver = \"waiter\"\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<Waiter>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

/// Ticks the driver has had. Read here, not by a method call: a method call is itself followed by
/// a tick (the contract's rule), so reading through one would tick the driver it counts.
fn ticked() -> u64 {
    TICKED.load(Ordering::SeqCst)
}

/// Runs what is due, until `done` says so or two seconds pass.
fn run_until(host: &Host, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        host.run_ticks();
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

#[test]
fn a_wake_of_the_descriptor_ticks_the_driver_once_and_nothing_else_does() {
    let (host, _clock) = host();
    let mut peer = PEER
        .get()
        .and_then(|peer| peer.lock().unwrap().take())
        .expect("the driver made its descriptor when it started");

    // The first tick, due at once after the start.
    assert!(run_until(&host, || ticked() == 1));

    // One wake: one more tick, from the descriptor, not from a deadline (there is none).
    peer.write_all(&[1]).unwrap();
    assert!(
        run_until(&host, || ticked() == 2),
        "a wake of the descriptor ticks the driver"
    );

    // No wake, no tick: the host has cleared the one it read, so nothing is left to spin on.
    for _ in 0..50 {
        host.run_ticks();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(ticked(), 2, "a quiet descriptor ticks nothing");

    // A second wake is a second tick.
    peer.write_all(&[1]).unwrap();
    assert!(run_until(&host, || ticked() == 3));
}
