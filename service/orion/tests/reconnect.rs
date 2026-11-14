//! A comm core that goes away and comes back, on a short heartbeat: the bridge reconnects, and its
//! topics, its resources and `describe` answer again without waiting for any change. This binary
//! runs this one test alone, so its log (`lemnos-orion:` lines, and the `reconnect:` lines the test
//! writes) is one session's, with nothing interleaved.
#![allow(clippy::print_stderr)]

use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lemnos_driver_system_identity::Identity;
use lemnos_fake_devices::{FakeAccelerometer, FakeCounter};
use lemnos_host::board::parse;
use lemnos_host::{Options as HostOptions, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, calls, publish, source};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion::actions::{ActionCall, Finished, Outcome, Target};
use orion::provider::{Found, ResourceState};
use orion::topics::{FieldId, Subscriber};
use orion::{Address, Orion};
use orion_node_app::{Node, NodeSettings};
use orion_stubs::{Board, Options as BoardOptions};
use tokio::sync::oneshot;

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [[devices]]\nid = \"imu\"\ndriver = \"fake-accelerometer\"\n\
    [devices.config]\nx = 1.5\ny = 0\nz = 2\n\
    [[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n[devices.config]\nstart = 41\n";

const WAIT: Duration = Duration::from_secs(20);

/// Short enough that a loaded machine misses the three heartbeats now and then; the restarts below
/// do not depend on it.
const HEARTBEAT_MS: u32 = 200;
/// The comm core's idle time for these runs: `comm.idle_ms` is zero, so it is three heartbeats. A
/// session the comm core has not heard from for this long is closed, and its names freed.
const IDLE: Duration = Duration::from_millis(3 * HEARTBEAT_MS as u64);
/// What the bound adds to idle once the link is back: one heartbeat, for the bridge's retry cadence
/// and the test's polls. A bridge that backs off instead (1 s, then longer) misses it.
const MARGIN: Duration = Duration::from_millis(HEARTBEAT_MS as u64);
/// How long the comm core still hears the bridge after the bridge stops hearing the comm core: two
/// heartbeats. The comm core closes the session `IDLE` after its own last word from the bridge, so
/// the bridge gives up and reconnects while the comm core still holds the old session for about
/// this long. That is the window in which a new session is refused, as it is on a loaded machine.
const LAG: Duration = Duration::from_millis(2 * HEARTBEAT_MS as u64);
/// After the link returns (the bridge's connection is accepted), the topic, describe and a call
/// answer within this: the old session is gone within `IDLE` of the bridge's last word from it,
/// and the bridge's retry comes on a short interval until then.
/// How long past the bound the test keeps asking before it fails, so a late answer is measured.
const SLACK: Duration = Duration::from_secs(2);
const BOUND: Duration = Duration::from_millis(IDLE.as_millis() as u64 + MARGIN.as_millis() as u64);

/// The comm core's options for every start: a short heartbeat, and the same place each time.
fn board_options() -> BoardOptions {
    BoardOptions {
        for_node: true,
        heartbeat_ms: HEARTBEAT_MS,
        ..BoardOptions::default()
    }
}

static NEXT: AtomicU32 = AtomicU32::new(0);

fn dir(test: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lo-{test}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
}

fn start_lemnosd(socket: &Path) -> Service {
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(BOARD).expect("board"),
        drivers: vec![
            entry::<FakeAccelerometer>(),
            entry::<FakeCounter>(),
            entry::<Identity>(),
        ],
        backend: Box::new(SimBoard::new()),
        options: HostOptions::default(),
    })
    .expect("lemnosd")
}

/// Orion node logic in a thread of its own, until dropped.
struct Logic {
    kill: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Logic {
    fn start(dir: &Path) -> Self {
        let mut source = orion_config::Source::parse("", []).expect("no settings");
        let mut settings = NodeSettings::from_source(&mut source).expect("defaults");
        settings.config.socket = dir.join("orion.sock");
        settings.config.state_dir = dir.join("state");
        settings.files_dir = dir.join("files");
        settings.audit = None;
        settings.config.node_name = "board".parse().expect("name");
        let (kill, killed) = oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let mut node = Node::load(settings).expect("node logic starts");
                tokio::select! {
                    () = node.run(std::future::pending()) => {}
                    _ = killed => {}
                }
            });
        });
        Self {
            kill: Some(kill),
            thread: Some(thread),
        }
    }
}

impl Drop for Logic {
    fn drop(&mut self) {
        if let (Some(kill), Some(thread)) = (self.kill.take(), self.thread.take()) {
            let _ = kill.send(());
            let _ = thread.join();
        }
    }
}

/// A unix-socket relay between the bridge and the comm core. The test stalls the connections it has
/// accepted so far: the bridge then hears nothing, and gives its session up when its idle timer
/// runs out, while the comm core still holds that session (its socket is open, nothing closed). The
/// comm core hears the bridge for a lag after that (see [`LAG`]), so it holds the session for about
/// that long after the bridge gives up. A connection accepted after the stall is relayed as normal,
/// which is the bridge's reconnect.
struct Relay {
    stop: Arc<AtomicBool>,
    stall: Arc<Mutex<Arc<Stall>>>,
    /// When each connection was accepted, in order.
    accepted: Arc<Mutex<Vec<Instant>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Relay {
    fn start(listen: &Path, to: PathBuf) -> Self {
        let _ = std::fs::remove_file(listen);
        let listener = UnixListener::bind(listen).expect("relay socket");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let stall = Arc::new(Mutex::new(Arc::new(Stall::default())));
        let accepted: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let (stop, stall, accepted) =
                (Arc::clone(&stop), Arc::clone(&stall), Arc::clone(&accepted));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            let flag = Arc::clone(&lock(&stall));
                            lock(&accepted).push(Instant::now());
                            let (to, stop) = (to.clone(), Arc::clone(&stop));
                            std::thread::spawn(move || relay(client, &to, &flag, &stop));
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            stop,
            stall,
            accepted,
            thread: Some(thread),
        }
    }

    /// Stalls every connection accepted so far, and makes the next one a normal relay. The comm
    /// core's words to the bridge stop at once; the bridge's words to the comm core stop `lag` later.
    fn stall(&self, lag: Duration) {
        let mut current = lock(&self.stall);
        let old = Arc::clone(&current);
        old.down.store(true, Ordering::Release);
        *current = Arc::new(Stall::default());
        drop(current);
        std::thread::spawn(move || {
            std::thread::sleep(lag);
            old.up.store(true, Ordering::Release);
        });
    }

    /// How many connections have been accepted: a new one is a session connecting again.
    fn accepted(&self) -> u32 {
        u32::try_from(lock(&self.accepted).len()).unwrap_or(u32::MAX)
    }

    /// When connection number `index` (from zero) was accepted, if it has been.
    fn accepted_at(&self, index: u32) -> Option<Instant> {
        lock(&self.accepted).get(index as usize).copied()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Which way of a connection is stalled. `down` is the comm core's words to the bridge, `up` the
/// bridge's words to the comm core. A stalled direction is not read, so its bytes wait in the socket.
#[derive(Default)]
struct Stall {
    down: AtomicBool,
    up: AtomicBool,
}

/// One connection, relayed both ways while it is not stalled. When one direction ends, both sockets
/// close.
fn relay(client: UnixStream, to: &Path, stall: &Stall, stop: &AtomicBool) {
    let Ok(server) = UnixStream::connect(to) else {
        return;
    };
    for stream in [&client, &server] {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
    }
    let Ok(closer) = client
        .try_clone()
        .and_then(|c| Ok((c, server.try_clone()?)))
    else {
        return;
    };
    std::thread::scope(|scope| {
        scope.spawn(|| pump(&client, &server, &stall.up, stop));
        pump(&server, &client, &stall.down, stop);
        let _ = closer.0.shutdown(std::net::Shutdown::Both);
        let _ = closer.1.shutdown(std::net::Shutdown::Both);
    });
}

/// Bytes and the file descriptors that come with them, one `recvmsg` at a time: Orion's transport
/// passes descriptors (the shared rings and their doorbells) with the frames they belong to, so a
/// relay that copied only the bytes would lose them.
const FDS: usize = 16;

fn pump(from: &UnixStream, to: &UnixStream, stall: &AtomicBool, stop: &AtomicBool) {
    let mut buffer = vec![0u8; 16 * 1024];
    let mut received_space =
        [std::mem::MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(FDS))];
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if stall.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let mut iov = [std::io::IoSliceMut::new(&mut buffer)];
        let mut received = rustix::net::RecvAncillaryBuffer::new(&mut received_space);
        let bytes = match rustix::net::recvmsg(
            from,
            &mut iov,
            &mut received,
            rustix::net::RecvFlags::empty(),
        ) {
            Ok(message) => message.bytes,
            Err(error)
                if matches!(
                    std::io::Error::from(error).kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return,
        };
        let mut fds = Vec::new();
        for message in received.drain() {
            if let rustix::net::RecvAncillaryMessage::ScmRights(incoming) = message {
                fds.extend(incoming);
            }
        }
        if bytes == 0 && fds.is_empty() {
            return;
        }
        let borrowed: Vec<std::os::fd::BorrowedFd<'_>> =
            fds.iter().map(std::os::fd::AsFd::as_fd).collect();
        let mut sent = 0;
        let mut first = true;
        while sent < bytes {
            let mut sent_space =
                [std::mem::MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(FDS))];
            let mut out = rustix::net::SendAncillaryBuffer::new(&mut sent_space);
            if first && !borrowed.is_empty() {
                out.push(rustix::net::SendAncillaryMessage::ScmRights(&borrowed));
            }
            match rustix::net::sendmsg(
                to,
                &[std::io::IoSlice::new(&buffer[sent..bytes])],
                &mut out,
                rustix::net::SendFlags::empty(),
            ) {
                Ok(n) => {
                    sent += n;
                    first = false;
                }
                Err(_) => return,
            }
        }
    }
}

/// The bridge's three threads.
struct Bridge {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn start_bridge(lemnosd: &Path, orion: &Path) -> Bridge {
    let shared = Arc::new(Shared::new().expect("shared"));
    let config = Config {
        lemnosd_socket: lemnosd.to_path_buf(),
        orion: Address::Local(orion.to_path_buf()),
        node_id: None,
        run_as: None,
        files: Vec::new(),
        links: Vec::new(),
        streams: Vec::new(),
        lease: Some(lemnos_orion::config::LEASE),
        stall: lemnos_orion::files::IDLE,
    };
    let (jobs, queue) = mpsc::channel();
    let threads = vec![
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || source::run(&socket, &shared))
        },
        {
            let (socket, shared) = (lemnosd.to_path_buf(), Arc::clone(&shared));
            std::thread::spawn(move || {
                calls::run(&socket, &queue, &shared, Some(lemnos_orion::config::LEASE))
            })
        },
        {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || publish::run(&config, &shared, &jobs))
        },
    ];
    Bridge { shared, threads }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shared.stop();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Polls `check` until it returns `Some`, or panics at the deadline.
fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn dashboard(orion: &Path) -> Orion {
    wait_for("a dashboard session", || {
        Orion::connect(&Address::Local(orion.to_path_buf()), "dashboard").ok()
    })
}

fn resource(dashboard: &Orion, name: &str) -> Option<Found> {
    dashboard
        .resources(name)
        .ok()?
        .into_iter()
        .find(|found| found.name == name)
}

fn latest(subscriber: &mut Subscriber, field: u16) -> Option<Vec<u8>> {
    let _ = subscriber.wait(Duration::from_millis(50));
    subscriber
        .latest(FieldId(field))
        .map(|sample| sample.payload.to_vec())
}

/// The generation the imu's description topic carries, once it has one.
fn topic_generation(subscriber: &mut Subscriber) -> Option<i64> {
    let bytes = latest(subscriber, 0)?;
    Some(i64::from_le_bytes(bytes.try_into().ok()?))
}

fn run(dashboard: &Orion, action: &str, resource: &str, args: &str) -> Finished {
    let mut run = dashboard
        .run(&ActionCall {
            name: action,
            target: Target::Resource(resource.parse().expect("name")),
            args: args.as_bytes(),
            deadline_ms: 0,
            detached: false,
        })
        .expect("run");
    run.finish(WAIT)
        .unwrap_or_else(|error| panic!("{action} {args:?} on {resource}: {error:?}"))
}

/// The generation `describe` gives the imu, from its device record.
fn described_generation(dashboard: &Orion) -> i64 {
    let done = run(dashboard, "lemnos.imu.call", "lemnos.imu", "describe");
    assert_eq!(done.outcome, Outcome::Succeeded, "{done:?}");
    generation_of(&done.output).expect("a generation")
}

fn generation_of(output: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(output).ok()?;
    let records = lemnos_orion::description::parse(text).ok()?;
    let device = records.iter().find(|r| r.word == "device")?;
    device.get("generation")?.parse().ok()
}

/// One run of `action` on `resource`, if it finishes within `wait`; a refusal or a run that does not
/// finish is `None`.
fn try_run(
    dashboard: &Orion,
    action: &str,
    resource: &str,
    args: &str,
    wait: Duration,
) -> Option<Finished> {
    let mut run = dashboard
        .run(&ActionCall {
            name: action,
            target: Target::Resource(resource.parse().ok()?),
            args: args.as_bytes(),
            deadline_ms: 0,
            detached: false,
        })
        .ok()?;
    run.finish(wait).ok()
}

/// Whether the imu answers at `first` now, through a session of its own: its topic carries that
/// generation, `describe` gives it, and a call succeeds. A topic or resource the lost session still
/// holds answers as well, but a run of its actions goes nowhere, so the calls are what count.
fn answering(board: &Path, first: i64) -> bool {
    let Ok(dash) = Orion::connect(&Address::Local(board.to_path_buf()), "dashboard") else {
        return false;
    };
    let Ok(mut topic) = dash.subscribe("lemnos.imu.description") else {
        return false;
    };
    if topic_generation(&mut topic) != Some(first) {
        return false;
    }
    let one_call = Duration::from_secs(1);
    let described = try_run(&dash, "lemnos.imu.call", "lemnos.imu", "describe", one_call);
    let Some(described) = described.filter(|done| done.outcome == Outcome::Succeeded) else {
        return false;
    };
    if generation_of(&described.output) != Some(first) {
        return false;
    }
    try_run(&dash, "lemnos.imu.call", "lemnos.imu", "describe", one_call)
        .is_some_and(|done| done.outcome == Outcome::Succeeded)
}

/// A session the bridge loses while the comm core still holds it (the link is stalled, not closed):
/// the bridge connects again, and its topics, its resources and `describe` answer again without any
/// change in the device. Each step waits on the event it depends on: the relay accepting the new
/// connection, and then the topic, describe and a call.
#[test]
fn a_session_lost_while_the_comm_core_holds_it_is_published_again() {
    eprintln!("reconnect: test a_session_lost... starts");
    let dir = dir("reconnect");
    let lemnosd = dir.join("lemnosd.sock");
    let board = Board::start_with(&dir, board_options());
    let _logic = Logic::start(&dir);
    let _service = start_lemnosd(&lemnosd);
    let board_socket = board.socket().to_path_buf();
    let relay = Relay::start(&dir.join("relay.sock"), board_socket.clone());
    let _bridge = start_bridge(&lemnosd, &dir.join("relay.sock"));
    let dash = dashboard(&board_socket);
    // Subscribed as soon as the topic is there, as the live walk does: a topic's samples are said
    // once, and a subscriber that comes later may miss the first.
    let mut topic = wait_for("the imu's description topic", || {
        dash.subscribe("lemnos.imu.description").ok()
    });
    let first = wait_for("the imu's description generation", || {
        topic_generation(&mut topic)
    });
    wait_for("the imu's resource", || {
        resource(&dash, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    assert_eq!(
        described_generation(&dash),
        first,
        "describe before the stall"
    );
    eprintln!("reconnect: first session: generation {first}, describe agrees");

    for cycle in 1..=3 {
        let next = relay.accepted();
        eprintln!("reconnect: cycle {cycle}: the link stalls; the comm core keeps the session");
        relay.stall(LAG);
        eprintln!("reconnect: cycle {cycle}: waiting for the bridge to connect again");
        // The test's own session is used while it waits, so the comm core does not take it for idle.
        // The link is back when the bridge's next connection is accepted: the bound runs from that
        // moment, not from when the test happens to look.
        let returned = wait_for("the bridge connecting again", || {
            let _ = resource(&dash, "lemnos.imu");
            relay.accepted_at(next)
        });
        eprintln!("reconnect: cycle {cycle}: the bridge connected again; waiting on the answer");
        let deadline = returned + BOUND + SLACK;
        let back = loop {
            if answering(&board_socket, first) {
                break returned.elapsed();
            }
            assert!(
                Instant::now() < deadline,
                "cycle {cycle}: the topic, describe and a call did not answer within {BOUND:?} of \
                 the link returning (idle {IDLE:?} + margin {MARGIN:?}); still not at {:?}",
                returned.elapsed()
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        eprintln!(
            "reconnect: cycle {cycle}: topic, describe and a call answer at generation {first}, \
             {back:?} after the link returned (bound {BOUND:?})"
        );
        assert!(
            back <= BOUND,
            "cycle {cycle}: the topic, describe and a call came back {back:?} after the link \
             returned, over the bound {BOUND:?} (idle {IDLE:?} + margin {MARGIN:?})"
        );
    }
}
