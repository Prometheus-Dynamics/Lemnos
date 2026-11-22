//! A call whose reply is lost: the client's deadline passes while the device still applies the
//! call. The client gets a timeout, and nothing in the existing API says whether the call applied.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lemnos_client::{Client, Error};
use lemnos_core::{Config as DriverConfig, Lifecycle, Result, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

/// How long the next `mark` takes, and whether it has applied.
static DELAY_MS: AtomicU64 = AtomicU64::new(0);
static APPLIED: AtomicBool = AtomicBool::new(false);
/// The tests share the statics above, so each one holds this for its whole run.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

#[contract("slow", version = 1)]
pub trait Slow {
    #[method(1)]
    fn mark(&mut self) -> Result<()>;
    #[method(2)]
    fn applied(&mut self) -> Result<u32>;
}

#[driver("slow", provides = [Slow])]
pub struct SlowDriver {
    marks: u32,
}

impl Lifecycle for SlowDriver {
    type Needs = SlowDriverNeeds;

    fn start(_: SlowDriverNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self { marks: 0 })
    }
}

impl Slow for SlowDriver {
    fn mark(&mut self) -> Result<()> {
        self.marks += 1;
        std::thread::sleep(Duration::from_millis(DELAY_MS.load(Ordering::SeqCst)));
        APPLIED.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn applied(&mut self) -> Result<u32> {
        Ok(u32::from(APPLIED.load(Ordering::SeqCst)))
    }
}

struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn start(tag: &str) -> (Service, Dir, std::path::PathBuf) {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, Into::into);
    let dir = base.join(format!("lemnosd-reply-lost-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = Dir(dir.clone());
    let socket = dir.join("sock");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                [[devices]]\nid = \"s\"\ndriver = \"slow\"\n";
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(text).unwrap(),
        drivers: vec![entry::<SlowDriver>()],
        backend: Box::new(SimBoard::new()),
        options: Options {
            call_deadline: Duration::from_secs(10),
            ..Options::default()
        },
    })
    .unwrap();
    (service, guard, socket)
}

#[test]
fn a_reply_lost_past_the_client_deadline_still_applies_and_the_client_cannot_tell() {
    let _one = one_at_a_time();
    let (_service, _dir, socket) = start("lost");
    DELAY_MS.store(3_000, Ordering::SeqCst);
    APPLIED.store(false, Ordering::SeqCst);

    let caller = Client::connect(&socket, "caller").unwrap();
    caller.set_timeout(Duration::from_secs(1));
    let mut handle = caller.open_tag("s", "slow/1").unwrap();
    let mut reply = [0u8; 8];
    let started = Instant::now();
    let error = caller.call(&mut handle, 1, &[], &mut reply).unwrap_err();
    assert_eq!(
        error,
        Error::TimedOut,
        "the client gives up at its deadline"
    );
    assert!(started.elapsed() < Duration::from_secs(2));

    // A second client sees that the device applied the call the first one gave up on.
    let other = Client::connect(&socket, "other").unwrap();
    let mut look = other.open_tag("s", "slow/1").unwrap();
    let mut value = [0u8; 4];
    let n = other.call(&mut look, 2, &[], &mut value).unwrap();
    assert_eq!(n, 4);
    assert_eq!(
        u32::from_le_bytes(value),
        1,
        "the device applied the lost call"
    );
}
