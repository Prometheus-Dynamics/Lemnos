//! A TCP relay between the bridge and a comm core, on 127.0.0.1. It forwards bytes both ways while
//! a connection is not stalled, and can stall, resume and close the connections it has accepted.
//! Every failure it makes is a real socket one, not a flag the bridge reads: a stall is a socket
//! that is open and not read, so the bytes wait in the kernel's buffers; a close is `close(2)` on
//! the socket, which sends a FIN, or a reset when bytes the relay has not read are still in it.
//! Nothing here is a clock: the events the tests wait on are the relay's counts and flags.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One relayed connection's flags. `down` is the comm core's bytes to the bridge, `up` the
/// bridge's bytes to the comm core: a stalled direction is not read.
#[derive(Default)]
struct Conn {
    down: AtomicBool,
    up: AtomicBool,
    /// Set by the test: the relay drops the connection now.
    close: AtomicBool,
    /// Set by the relay when a stalled bridge-side socket holds bytes it has not read.
    unread: AtomicBool,
    /// Both sides are finished and the sockets are closed.
    done: AtomicBool,
}

#[derive(Default)]
struct State {
    live: Mutex<Vec<Arc<Conn>>>,
    accepted: AtomicU32,
    ended: AtomicU32,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// The relay: listens on 127.0.0.1 at a port of its own choosing, and forwards every connection
/// to `to`. Dropping it stops accepting and drops what is open.
pub struct TcpRelay {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    state: Arc<State>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TcpRelay {
    /// Starts a relay to `to`. The listening port is read back from the socket.
    pub fn start(to: SocketAddr) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("relay listener");
        let addr = listener.local_addr().expect("relay address");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(State::default());
        let thread = {
            let (stop, state) = (Arc::clone(&stop), Arc::clone(&state));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            let conn = Arc::new(Conn::default());
                            lock(&state.live).push(Arc::clone(&conn));
                            state.accepted.fetch_add(1, Ordering::AcqRel);
                            let (stop, state) = (Arc::clone(&stop), Arc::clone(&state));
                            std::thread::spawn(move || {
                                relay(client, to, &conn, &stop);
                                conn.done.store(true, Ordering::Release);
                                state.ended.fetch_add(1, Ordering::AcqRel);
                            });
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
            addr,
            stop,
            state,
            thread: Some(thread),
        }
    }

    /// Where the bridge connects: `127.0.0.1` and the port the relay listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Connections accepted so far. A new one is the bridge connecting again.
    pub fn accepted(&self) -> u32 {
        self.state.accepted.load(Ordering::Acquire)
    }

    /// Connections whose sockets are closed: both sides finished.
    pub fn ended(&self) -> u32 {
        self.state.ended.load(Ordering::Acquire)
    }

    /// Stalls every open connection in both directions. Its sockets stay open and nothing is read
    /// from them, so the bytes wait in the kernel's buffers. A connection accepted after this is
    /// relayed as normal: that is the bridge's new connection.
    pub fn stall(&self) {
        self.live(|conn| {
            conn.down.store(true, Ordering::Release);
            conn.up.store(true, Ordering::Release);
        });
    }

    /// Lets every open connection carry on from where it stopped.
    pub fn resume(&self) {
        self.live(|conn| {
            conn.down.store(false, Ordering::Release);
            conn.up.store(false, Ordering::Release);
        });
    }

    /// Closes every open connection now, stalled or not: its socket is closed, so the bridge's end
    /// is closed by the kernel (a FIN, or a reset if bytes the relay had not read were in it).
    pub fn close(&self) {
        self.live(|conn| conn.close.store(true, Ordering::Release));
    }

    /// Whether an open connection holds bytes from the bridge that the relay has not read while
    /// stalled. The relay finds out by peeking at the socket, which does not take the bytes.
    pub fn unread(&self) -> bool {
        lock(&self.state.live)
            .iter()
            .any(|c| c.unread.load(Ordering::Acquire))
    }

    /// Runs `change` on each open connection, and forgets the finished ones.
    fn live(&self, change: impl Fn(&Conn)) {
        let mut live = lock(&self.state.live);
        live.retain(|conn| !conn.done.load(Ordering::Acquire));
        for conn in live.iter() {
            change(conn);
        }
    }
}

impl Drop for TcpRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One connection, relayed both ways while it is not stalled. When one side ends, both sockets are
/// shut down so the other direction ends too. A close drops the sockets as they are.
fn relay(client: TcpStream, to: SocketAddr, conn: &Conn, stop: &AtomicBool) {
    let Ok(server) = TcpStream::connect(to) else {
        return;
    };
    for stream in [&client, &server] {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(10)));
        let _ = stream.set_nodelay(true);
    }
    let (Ok(client_end), Ok(server_end)) = (client.try_clone(), server.try_clone()) else {
        return;
    };
    std::thread::scope(|scope| {
        scope.spawn(|| pump(&client, &server, &conn.up, conn, stop, true));
        pump(&server, &client, &conn.down, conn, stop, false);
    });
    // A side that ended shuts both ways, so the other direction ends too. A close does not: the
    // sockets are dropped as they are, which the kernel answers with a reset if bytes are unread.
    if !conn.close.load(Ordering::Acquire) {
        let _ = client_end.shutdown(std::net::Shutdown::Both);
        let _ = server_end.shutdown(std::net::Shutdown::Both);
    }
}

/// Moves bytes from `from` to `to` while `stall` is off, and ends when `from` ends, the connection is
/// closed, or the relay stops. `bridge_side` is set on the bridge's direction, where a stalled socket
/// is peeked at for bytes it holds.
fn pump(
    from: &TcpStream,
    to: &TcpStream,
    stall: &AtomicBool,
    conn: &Conn,
    stop: &AtomicBool,
    bridge_side: bool,
) {
    let mut buffer = vec![0u8; 16 * 1024];
    let mut reader = from;
    loop {
        if stop.load(Ordering::Acquire) || conn.close.load(Ordering::Acquire) {
            return;
        }
        if stall.load(Ordering::Acquire) {
            if bridge_side {
                let mut peek = [0u8; 1];
                if from.peek(&mut peek).is_ok_and(|n| n > 0) {
                    conn.unread.store(true, Ordering::Release);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let bytes = match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => n,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                continue;
            }
            Err(_) => return,
        };
        let mut writer = to;
        let mut sent = 0;
        while sent < bytes {
            if stop.load(Ordering::Acquire) || conn.close.load(Ordering::Acquire) {
                return;
            }
            match writer.write(&buffer[sent..bytes]) {
                Ok(n) => sent += n,
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return,
            }
        }
    }
}
