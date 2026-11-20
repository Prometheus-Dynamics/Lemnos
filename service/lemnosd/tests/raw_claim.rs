//! Raw claims through the real service: a client process claims a raw device, drives its line and
//! PWM channel, and is killed with `SIGKILL`. lemnosd ends the claim when the connection ends: the
//! line and the channel go to the board's safe state, and another client can claim. While the claim
//! stands, the other client is refused (`BUSY`) and still reads the line's edges.
//!
//! The bound, `BOUND`: as for a fan's hand-back (`caller_gone_hand_back.rs`), lemnosd's poll wakes on
//! the connection's end at once and tells the driver through its lane; the driver sets the safe state
//! in that call. One second is the lane's deadline plus margin. The measured time is printed.
//!
//! The client is this test binary run again (see [`a_client_that_claims_and_waits`]).

#![allow(clippy::print_stdout)]

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::start_board;
use lemnos_client::Client;
use lemnos_contract_raw_bus::{
    BIAS_AS_IS, DRIVE_PUSH_PULL, EDGE_BOTH, EDGE_BYTES, EDGE_NONE, LINE_INPUT, LINE_OUTPUT,
    MAX_EDGES, RawBus, decode_edge,
};
use lemnos_core::Status;
use lemnos_driver_raw_bus::RawBus as RawBusDriver;
use lemnos_hal::raw::{Direction, LineConfig};
use lemnos_host::entry;
use lemnos_sim::SimBoard;

const BOUND: Duration = Duration::from_secs(1);
const CLIENT_TEST: &str = "a_client_that_claims_and_waits";
const CLIENT_SOCKET: &str = "LEMNOS_RAW_CLIENT_SOCKET";
const CLAIMED: &str = "RAW-CLIENT CLAIMED";

const BOARD: &str = "[[devices]]\nid = \"aux\"\ndriver = \"raw-bus\"\n\
    uses = { line = \"aux-line\", pwm = \"buzzer\" }\n\
    config = { writes = true, line_safe = \"low\" }\n";

/// Not a test of its own: started as the client, it claims `aux`, drives the line high and the PWM
/// channel on, and waits to be killed.
#[test]
fn a_client_that_claims_and_waits() {
    let Ok(socket) = std::env::var(CLIENT_SOCKET) else {
        return;
    };
    let client = Client::connect(Path::new(&socket), "raw-client").unwrap();
    let mut aux = client.open::<dyn RawBus>("aux").unwrap();
    aux.claim().unwrap();
    aux.line_configure(
        LINE_OUTPUT,
        true,
        false,
        BIAS_AS_IS,
        DRIVE_PUSH_PULL,
        EDGE_NONE,
        0,
    )
    .unwrap();
    aux.pwm_set(40_000, 20_000, true).unwrap();
    println!("{CLAIMED}");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_client(socket: &Path) -> Killed {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CLIENT_TEST, "--nocapture", "--test-threads=1"])
        .env(CLIENT_SOCKET, socket)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if line.contains(CLAIMED) {
                let _ = tx.send(());
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(20))
        .expect("the client never claimed");
    Killed(child)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_killed_clients_claim_ends_in_the_safe_state_and_another_client_claims() {
    let sim = SimBoard::new();
    let line = sim.add_line("aux-line");
    let pwm = sim.add_pwm("buzzer");
    let running = start_board(BOARD, vec![entry::<RawBusDriver>()], sim, None);
    let client = Client::connect(&running.socket, "test").unwrap();
    wait_for("aux is bound", || client.open::<dyn RawBus>("aux").is_ok());
    let mut aux = client.open::<dyn RawBus>("aux").unwrap();

    let mut child = spawn_client(&running.socket);
    assert_eq!(line.level(), Some(true));
    assert!(pwm.current().enabled);
    // The other client: refused while the claim stands, reading still open.
    assert_eq!(aux.claim(), Err(Status::BUSY));
    assert_eq!(aux.pwm_set(40_000, 0, false), Err(Status::BUSY));
    assert_eq!(aux.line_get(), Ok(true));

    let started = Instant::now();
    let _ = child.0.kill();
    let _ = child.0.wait();
    loop {
        if line.config() == LineConfig::output(false) && !pwm.current().enabled {
            println!(
                "raw claim ended after SIGKILL: {} ms (bound {} ms)",
                started.elapsed().as_millis(),
                BOUND.as_millis()
            );
            break;
        }
        assert!(
            started.elapsed() < BOUND,
            "the line stayed {:?} and the PWM {:?} for the {} ms bound after its client was killed",
            line.config(),
            pwm.current(),
            BOUND.as_millis()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(line.level(), Some(false));
    assert_eq!(aux.claim(), Ok(()));
}

#[test]
fn edges_of_a_claimed_line_reach_a_subscriber_and_any_reader() {
    let sim = SimBoard::new();
    let line = sim.add_line("aux-line");
    let _pwm = sim.add_pwm("buzzer");
    let running = start_board(BOARD, vec![entry::<RawBusDriver>()], sim, None);
    let client = Client::connect(&running.socket, "test").unwrap();
    wait_for("aux is bound", || client.open::<dyn RawBus>("aux").is_ok());
    let mut aux = client.open::<dyn RawBus>("aux").unwrap();
    aux.claim().unwrap();
    aux.line_configure(
        LINE_INPUT,
        false,
        false,
        BIAS_AS_IS,
        DRIVE_PUSH_PULL,
        EDGE_BOTH,
        0,
    )
    .unwrap();
    assert_eq!(line.config().direction, Direction::Input);
    line.edge(true, 10);
    line.edge(false, 20);

    let other = Client::connect(&running.socket, "reader").unwrap();
    let mut seen = other.open::<dyn RawBus>("aux").unwrap();
    let mut into = [0u8; MAX_EDGES * EDGE_BYTES];
    let n = seen.read_edges(&mut into).unwrap();
    let edges: Vec<_> = into[..n]
        .chunks(EDGE_BYTES)
        .map(|b| decode_edge(b).unwrap())
        .map(|e| (e.rising, e.seq, e.timestamp_ns))
        .collect();
    assert_eq!(edges, [(true, 1, 10), (false, 2, 20)]);

    // A subscription to read_edges (method 13): Lemnos's own reads, which no client claim covers,
    // carry the ring; a new edge is a new value.
    let mut handle = other.open_tag("aux", "raw-bus/1").unwrap();
    other
        .subscribe(&mut handle, READ_EDGES, 10_000, true)
        .unwrap();
    let mut value = [0u8; MAX_EDGES * EDGE_BYTES];
    let last_seq =
        |value: &[u8], len: usize| decode_edge(&value[len - EDGE_BYTES..len]).map(|e| e.seq);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut first = None;
    while first.is_none() {
        assert!(Instant::now() < deadline, "no edge value came");
        if let Some(info) = other
            .next_value(&mut value, Duration::from_millis(10))
            .unwrap()
        {
            first = last_seq(&value, info.len);
        }
    }
    assert_eq!(first, Some(2));
    line.edge(true, 30);
    loop {
        assert!(Instant::now() < deadline, "the new edge never came");
        if let Some(info) = other
            .next_value(&mut value, Duration::from_millis(10))
            .unwrap()
            && last_seq(&value, info.len) == Some(3)
        {
            break;
        }
    }
}

/// `read_edges`'s method number in `raw-bus/1`.
const READ_EDGES: u16 = 13;
