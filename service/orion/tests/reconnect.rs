//! A comm core that goes away and comes back, on a short heartbeat: the bridge reconnects, and its
//! topics, its resources and `describe` answer again without waiting for any change. This binary
//! runs this one test alone, so its log (`lemnos-orion:` lines, and the `reconnect:` lines the test
//! writes) is one session's, with nothing interleaved.
#![allow(clippy::print_stderr)]

use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use orion::provider::ResourceState;

#[allow(dead_code)]
#[path = "support/reconnect_kit.rs"]
mod kit;
use kit::*;

/// A unix-socket relay between the bridge and the comm core. The test stalls the connections it has
/// accepted so far: the bridge then hears nothing, and gives its session up when its idle timer
/// runs out, while the comm core still holds that session (its socket is open, nothing closed). The
/// comm core hears the bridge for a lag after that (see [`LAG`]), so it holds the session for about
/// that long after the bridge gives up. A connection accepted after the stall is relayed as normal,
/// which is the bridge's reconnect.
struct Relay {
    stop: Arc<AtomicBool>,
    stall: Arc<Mutex<Arc<Stall>>>,
    /// The stalled connections, until [`Relay::resume`].
    held: Mutex<Vec<Arc<Stall>>>,
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
            held: Mutex::new(Vec::new()),
            accepted,
            thread: Some(thread),
        }
    }

    /// Stalls every connection accepted so far, in both directions, and makes the next one a normal
    /// relay. Nothing is read in either direction until [`Relay::resume`]: the socket stays open.
    fn stall(&self) {
        let mut current = lock(&self.stall);
        let old = Arc::clone(&current);
        old.down.store(true, Ordering::Release);
        old.up.store(true, Ordering::Release);
        lock(&self.held).push(old);
        *current = Arc::new(Stall::default());
    }

    /// Lets the stalled connections carry on from where they stopped.
    fn resume(&self) {
        for stalled in lock(&self.held).drain(..) {
            stalled.down.store(false, Ordering::Release);
            stalled.up.store(false, Ordering::Release);
        }
    }

    /// How many connections have been accepted: a new one is a session connecting again.
    fn accepted(&self) -> u32 {
        u32::try_from(lock(&self.accepted).len()).unwrap_or(u32::MAX)
    }
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

/// A stalled link that stays open keeps its session. Nothing is torn down or published again while
/// the comm core and the bridge hear each other no more: the session is quiet, not gone, and the
/// bridge does not connect again. When the stall ends, publishing continues on the same session.
#[test]
fn a_stalled_open_link_keeps_its_session_and_publishing_resumes_on_it() {
    let dir = dir("stalled");
    let (board, _logic, _service, lemnosd) = start_relayed(&dir);
    let board_socket = board.socket().to_path_buf();
    let relay = Relay::start(&dir.join("relay.sock"), board_socket.clone());
    let _bridge = start_bridge(&lemnosd, &dir.join("relay.sock"));
    let dash = dashboard(&board_socket);
    let mut count = wait_for("the counter's first value", || {
        dash.subscribe("lemnos.thing.values").ok()
    });
    let before = wait_for("the counter has a value", || counter(&mut count));
    wait_for("the imu's resource", || {
        resource(&dash, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    assert_eq!(relay.accepted(), 1, "the first session");

    // The link stalls, open. Three times the comm core's idle time: a session that was closed by its
    // silence would be closed by now. The session is kept, so the resource is still there.
    relay.stall();
    std::thread::sleep(IDLE * 3);
    assert_eq!(
        relay.accepted(),
        1,
        "a stalled link made the bridge connect again"
    );
    assert!(
        resource(&dash, "lemnos.imu").is_some(),
        "a stalled link withdrew the bridge's resources"
    );

    // The stall ends: the same session carries on, and the counter moves on it.
    relay.resume();
    wait_for("publishing resumes after the stall", || {
        counter(&mut count).filter(|value| *value > before)
    });
    assert_eq!(relay.accepted(), 1, "publishing resumed on a new session");
}

/// A closed link is lost at once: the bridge's connection closes, and the bridge takes the session as
/// lost and connects again. The session it had is published again on the new connection. The test
/// waits for events, not for a time: the bridge connects again, then the resource and the description
/// are back; the outer deadline only catches a hang.
#[test]
fn a_closed_link_is_lost_and_the_session_is_published_again() {
    let dir = dir("closed");
    let (board, _logic, _service, lemnosd) = start_relayed(&dir);
    let board_socket = board.socket().to_path_buf();
    let relay_socket = dir.join("relay.sock");
    let relay = Relay::start(&relay_socket, board_socket.clone());
    let _bridge = start_bridge(&lemnosd, &relay_socket);
    let dash = dashboard(&board_socket);
    let mut topic = wait_for("the imu's description topic", || {
        dash.subscribe("lemnos.imu.description").ok()
    });
    let first = wait_for("the imu's description generation", || {
        topic_generation(&mut topic)
    });
    wait_for("the imu's resource", || {
        resource(&dash, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    assert_eq!(relay.accepted(), 1, "the first session");

    // The link closes: both of its connections, and the relay with them.
    drop(relay);
    // The link is back. The bridge connects again, and that is its new session.
    let relay = Relay::start(&relay_socket, board_socket.clone());
    wait_for("the bridge connects again after the link closed", || {
        (relay.accepted() >= 1).then_some(())
    });
    wait_for("the imu's resource is published again", || {
        resource(&dash, "lemnos.imu").filter(|found| found.state == ResourceState::Ready)
    });
    assert_eq!(
        described_generation(&dash),
        first,
        "the description after the link came back"
    );
}
