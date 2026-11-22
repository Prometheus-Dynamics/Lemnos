//! A call made with a token keeps its outcome at the service, so a client that gave up on the call
//! (its reply lost, its deadline passed, or its connection gone) can ask what became of it. These
//! run the real service and the real client over its socket.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lemnos_client::{Client, Error, Outcome};
use lemnos_core::{Config as DriverConfig, Lifecycle, Result, Status, contract, driver};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

/// How long the next `mark` takes, and whether it has applied.
static DELAY_MS: AtomicU64 = AtomicU64::new(0);
static APPLIED: AtomicBool = AtomicBool::new(false);
/// The tests share the statics above, so each holds this for its whole run.
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

/// A service whose driver's `mark` takes `delay_ms`, with the host's call deadline `deadline`.
fn start(tag: &str, deadline: Duration, delay_ms: u64) -> (Service, Dir, std::path::PathBuf) {
    DELAY_MS.store(delay_ms, Ordering::SeqCst);
    APPLIED.store(false, Ordering::SeqCst);
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, Into::into);
    let dir = base.join(format!("lemnosd-outcome-{tag}-{}", std::process::id()));
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
            call_deadline: deadline,
            ..Options::default()
        },
    })
    .unwrap();
    (service, guard, socket)
}

const MARK: u16 = 1;

/// Polls the outcome until it is not pending, for at most ten seconds.
fn settled(client: &mut Client, token: u64) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let outcome = client.outcome(token).unwrap();
        if outcome != Outcome::Pending {
            return outcome;
        }
        assert!(Instant::now() < deadline, "the call never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_reply_lost_at_the_client_is_read_back_as_applied_after_a_reconnect() {
    let _one = one_at_a_time();
    let (_service, _dir, socket) = start("lost", Duration::from_secs(10), 3_000);

    let caller = Client::connect(&socket, "caller").unwrap();
    caller.set_timeout(Duration::from_secs(1));
    let mut handle = caller.open_tag("s", "slow/1").unwrap();
    let mut reply = [0u8; 8];
    let error = caller
        .call_token(&mut handle, MARK, &[], &mut reply, 0x51)
        .unwrap_err();
    assert_eq!(
        error,
        Error::TimedOut,
        "the client gives up at its deadline"
    );
    drop(caller);

    // The same name, a new connection: the outcome is still there, and the call applied.
    let mut again = Client::connect(&socket, "caller").unwrap();
    assert_eq!(
        settled(&mut again, 0x51),
        Outcome::Applied(Status::OK),
        "the call applied, and the client can read that"
    );
    let mut look = again.open_tag("s", "slow/1").unwrap();
    let mut value = [0u8; 4];
    again.call(&mut look, 2, &[], &mut value).unwrap();
    assert_eq!(u32::from_le_bytes(value), 1);
}

#[test]
fn a_call_still_running_past_the_host_deadline_is_pending_then_applied() {
    let _one = one_at_a_time();
    let (_service, _dir, socket) = start("pending", Duration::from_secs(1), 3_000);

    let client = Client::connect(&socket, "caller").unwrap();
    let mut handle = client.open_tag("s", "slow/1").unwrap();
    let mut reply = [0u8; 8];
    let error = client
        .call_token(&mut handle, MARK, &[], &mut reply, 7)
        .unwrap_err();
    assert_eq!(
        error,
        Error::Refused {
            status: Status::TIMED_OUT,
            text: "the call failed".to_string(),
        }
    );

    let mut asking = Client::connect(&socket, "caller").unwrap();
    assert_eq!(asking.outcome(7).unwrap(), Outcome::Pending);
    assert_eq!(settled(&mut asking, 7), Outcome::Applied(Status::OK));
}

#[test]
fn a_token_whose_call_is_still_running_is_not_begun_again() {
    let _one = one_at_a_time();
    let (_service, _dir, socket) = start("again", Duration::from_secs(1), 2_000);

    let client = Client::connect(&socket, "caller").unwrap();
    let mut handle = client.open_tag("s", "slow/1").unwrap();
    let mut reply = [0u8; 8];
    client
        .call_token(&mut handle, MARK, &[], &mut reply, 9)
        .unwrap_err();
    let error = client
        .call_token(&mut handle, MARK, &[], &mut reply, 9)
        .unwrap_err();
    assert_eq!(
        error,
        Error::Refused {
            status: Status::BUSY,
            text: "a call with this token is still running".to_string(),
        },
        "the second call with the same token is not started"
    );
    let mut asking = Client::connect(&socket, "caller").unwrap();
    assert_eq!(settled(&mut asking, 9), Outcome::Applied(Status::OK));
}

#[test]
fn a_call_refused_before_its_driver_runs_is_not_applied() {
    let _one = one_at_a_time();
    let (_service, _dir, socket) = start("refused", Duration::from_secs(10), 0);

    let client = Client::connect(&socket, "caller").unwrap();
    let mut handle = client.open_tag("s", "slow/1").unwrap();
    let mut reply = [0u8; 8];
    let error = client
        .call_token(&mut handle, 99, &[], &mut reply, 11)
        .unwrap_err();
    assert!(matches!(error, Error::Refused { status, .. } if status == Status::NOT_SUPPORTED));
    assert_eq!(
        client.outcome(11).unwrap(),
        Outcome::NotApplied(Status::NOT_SUPPORTED)
    );
    assert!(!APPLIED.load(Ordering::SeqCst));
}

#[test]
fn an_unknown_token_is_unknown_and_so_is_another_clients_call() {
    let _one = one_at_a_time();
    let (_service, _dir, socket) = start("unknown", Duration::from_secs(10), 0);

    let mine = Client::connect(&socket, "mine").unwrap();
    assert_eq!(mine.outcome(0x1234).unwrap(), Outcome::Unknown);

    let client = Client::connect(&socket, "other").unwrap();
    let mut handle = client.open_tag("s", "slow/1").unwrap();
    let mut reply = [0u8; 8];
    client
        .call_token(&mut handle, MARK, &[], &mut reply, 0x77)
        .unwrap();
    assert_eq!(mine.outcome(0x77).unwrap(), Outcome::Unknown);
    assert_eq!(
        settled(&mut Client::connect(&socket, "other").unwrap(), 0x77),
        Outcome::Applied(Status::OK)
    );
}

#[test]
fn a_full_table_forgets_its_oldest_outcome_and_keeps_the_newest() {
    let _one = one_at_a_time();
    let (_service, _dir, socket) = start("full", Duration::from_secs(10), 0);

    let client = Client::connect(&socket, "filler").unwrap();
    let mut handle = client.open_tag("s", "slow/1").unwrap();
    let mut reply = [0u8; 8];
    let calls = 300;
    for token in 1..=calls {
        client
            .call_token(&mut handle, MARK, &[], &mut reply, token)
            .unwrap();
    }
    assert_eq!(client.outcome(1).unwrap(), Outcome::Unknown, "evicted");
    assert_eq!(client.outcome(calls).unwrap(), Outcome::Applied(Status::OK));
}
