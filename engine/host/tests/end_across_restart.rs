//! A lend's END kept across a restart of the process that runs the host ([`EndKeeper`]): the
//! write end of each hold is kept, so the holder's END does not read EOF while the process is
//! gone; the next host takes it back before any device binds, and the device's first tick says
//! whether the link was taken up as it was. If it was, the hold goes on as [`CallerId::BEFORE`]'s;
//! if not, it ends there. A hold whose holder has gone is given up when someone asks for the pair.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_core::{
    BufferHandle, CallerId, Config, ConfigError, Key, Kind, Lifecycle, Result, Status, contract,
    driver,
};
use lemnos_host::board::parse;
use lemnos_host::{EndKeeper, Host, Options, entry};
use lemnos_sim::SimBoard;

/// The host's warnings. Tests share the process: each looks for a line about its own device.
static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn listen() {
    lemnos_host::set_warning_sink(|line| HEARD.lock().unwrap().push(line.to_string()));
}

fn heard(device: &str, text: &str) -> bool {
    let start = format!("device {device} (driver kept-lender): ");
    HEARD
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.starts_with(&start) && line.contains(text))
}

/// Whether the next instance with this key takes its link up as it was.
static TAKES_UP: [AtomicBool; 4] = [const { AtomicBool::new(true) }; 4];

#[contract("kept-lend", version = 1)]
pub trait KeptLend {
    #[method(1)]
    fn nothing(&mut self) -> Result<u32>;
}

#[driver("kept-lender", provides = [KeptLend])]
pub struct KeptLender {
    holder: Option<CallerId>,
    pair: Arc<[OwnedFd; 2]>,
    took_up: bool,
}

static PAIR: Mutex<Option<Arc<[OwnedFd; 2]>>> = Mutex::new(None);

fn pair() -> Arc<[OwnedFd; 2]> {
    PAIR.lock()
        .unwrap()
        .get_or_insert_with(|| {
            let (a, b) = std::io::pipe().unwrap();
            Arc::new([OwnedFd::from(a), OwnedFd::from(b)])
        })
        .clone()
}

impl Lifecycle for KeptLender {
    type Needs = KeptLenderNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("key", Kind::Int)])
    }
    const TICKS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn start(_: KeptLenderNeeds, cfg: &Config<'_>) -> Result<Self> {
        let key = cfg.int("key").ok_or(Status::INVALID_ARGUMENT)? as usize;
        Ok(Self {
            holder: None,
            pair: pair(),
            took_up: TAKES_UP[key].load(Ordering::Relaxed),
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        Some(now_ns + 1_000_000)
    }

    fn took_up_link(&self) -> core::result::Result<(), &'static str> {
        if self.took_up {
            Ok(())
        } else {
            Err("the function changed")
        }
    }

    fn endpoints(&mut self, caller: CallerId) -> Result<[BufferHandle<'_>; 2]> {
        if self.holder.is_some_and(|holder| holder != caller) {
            return Err(Status::BUSY);
        }
        self.holder = Some(caller);
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

impl KeptLend for KeptLender {
    fn nothing(&mut self) -> Result<u32> {
        Ok(0)
    }
}

/// The service manager's store as the daemon uses it: a copy of each kept write end, by name,
/// given to the next process. While the process stops, nothing is let go (the daemon's stop).
#[derive(Default)]
struct Store {
    kept: Mutex<Vec<(String, OwnedFd)>>,
    stopping: AtomicBool,
    let_go: Mutex<Vec<String>>,
}

impl EndKeeper for Store {
    fn keep(&self, device: &str, write: BorrowedFd<'_>) {
        let copy = write.try_clone_to_owned().unwrap();
        self.kept.lock().unwrap().push((device.to_string(), copy));
    }

    fn let_go(&self, device: &str) {
        if self.stopping.load(Ordering::Relaxed) {
            return;
        }
        self.kept.lock().unwrap().retain(|(name, _)| name != device);
        self.let_go.lock().unwrap().push(device.to_string());
    }

    fn take(&self, device: &str) -> Option<OwnedFd> {
        // The next process gets copies; the store keeps its own until let go.
        let kept = self.kept.lock().unwrap();
        kept.iter()
            .find(|(name, _)| name == device)
            .map(|(_, fd)| fd.try_clone().unwrap())
    }

    fn readers_gone(&self, write: BorrowedFd<'_>) -> bool {
        let mut fds = [rustix::event::PollFd::new(
            &write,
            rustix::event::PollFlags::empty(),
        )];
        let now = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        rustix::event::poll(&mut fds, Some(&now))
            .is_ok_and(|_| fds[0].revents().contains(rustix::event::PollFlags::ERR))
    }
}

fn start(key: usize, store: &Arc<Store>) -> Host {
    listen();
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"k{key}\"\ndriver = \"kept-lender\"\nconfig = {{ key = {key} }}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start_keeping(
        "",
        &parse(&text).unwrap(),
        &[entry::<KeptLender>()],
        Box::new(sim),
        (Arc::new(clock), Options::default()),
        Arc::new(lemnos_host::stream::MemfdAllocator),
        Some(Arc::clone(store) as Arc<dyn EndKeeper>),
    )
    .unwrap();
    host.tick();
    host
}

fn index(host: &Host, key: usize) -> u32 {
    host.device(&format!("k{key}")).unwrap().index
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

const HOLDER: CallerId = CallerId(7);

/// The first process lends the pair with END, then stops as the daemon does with a store.
fn lent_then_stopped(key: usize, store: &Arc<Store>) -> OwnedFd {
    let host = start(key, store);
    let [_out, _input, end] = host.usb_endpoints_end(HOLDER, index(&host, key)).unwrap();
    assert_eq!(
        store.kept.lock().unwrap().len(),
        1,
        "kept as the hold began"
    );
    store.stopping.store(true, Ordering::Relaxed);
    host.shutdown();
    drop(host);
    store.stopping.store(false, Ordering::Relaxed);
    assert!(
        !over(&end),
        "the store's copy keeps END open while no process runs"
    );
    end
}

#[test]
fn a_hold_taken_up_as_it_was_goes_on_and_ends_when_its_holder_goes() {
    let store = Arc::new(Store::default());
    let end = lent_then_stopped(0, &store);
    let host = start(0, &store);
    std::thread::sleep(Duration::from_millis(1));
    host.tick();
    assert!(
        heard("k0", "the hold the daemon before lent is taken up"),
        "{:?} kept {}",
        HEARD.lock().unwrap(),
        store.kept.lock().unwrap().len()
    );
    assert!(!over(&end), "the holder's link goes on");
    // The pair is still the holder's: another caller is refused.
    assert_eq!(
        host.usb_endpoints_end(CallerId(8), index(&host, 0)).err(),
        Some(Status::BUSY)
    );
    // The holder goes (its last END copy closes): the next ask finds the hold orphaned.
    drop(end);
    let [_out, _input, again] = host
        .usb_endpoints_end(CallerId(8), index(&host, 0))
        .unwrap();
    assert!(heard(
        "k0",
        "the holder of the hold the daemon before lent has gone"
    ));
    assert!(!over(&again));
    assert_eq!(
        *store.let_go.lock().unwrap(),
        ["k0"],
        "the old hold's copy let go"
    );
}

#[test]
fn a_hold_whose_link_was_not_taken_up_ends_at_the_first_tick() {
    let store = Arc::new(Store::default());
    let end = lent_then_stopped(1, &store);
    TAKES_UP[1].store(false, Ordering::Relaxed);
    let host = start(1, &store);
    host.tick();
    assert!(heard("k1", "the hold the daemon before lent is dropped"));
    assert!(over(&end), "END reads EOF: the endpoints are new");
    assert!(store.kept.lock().unwrap().is_empty());
    // Free to lend again.
    assert!(host.usb_endpoints_end(CallerId(9), index(&host, 1)).is_ok());
}

#[test]
fn a_hold_taken_up_ends_for_good_when_the_device_is_switched_off() {
    let store = Arc::new(Store::default());
    let end = lent_then_stopped(2, &store);
    let host = start(2, &store);
    host.tick();
    assert!(!over(&end));
    host.off("k2").unwrap();
    assert!(over(&end));
    assert!(store.kept.lock().unwrap().is_empty());
}

#[test]
fn without_a_keeper_nothing_is_kept_and_a_stop_ends_the_hold() {
    listen();
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"k3\"\ndriver = \"kept-lender\"\nconfig = { key = 3 }\n";
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<KeptLender>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap();
    host.tick();
    let [_out, _input, end] = host.usb_endpoints_end(HOLDER, index(&host, 3)).unwrap();
    host.shutdown();
    drop(host);
    assert!(over(&end));
}
