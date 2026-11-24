//! The board's network link over the gadget's NCM function, end to end through the emulated bus.
//!
//! `dummy_hcd` loops the gadget back to a host controller in the same guest, so the board's
//! interface (`lemnos0`, the gadget's NCM function) and the host's (the `cdc_ncm` driver's) are both
//! here. Traffic between two addresses of one network namespace never leaves the machine, so the
//! host's interface is moved into a namespace of its own, held by a child of this check (`netpeer`):
//! from then on a TCP connection between the board's address and the host's crosses the USB link and
//! nothing else. The peer echoes what each connection sends.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The board's interface, by the default composition's `ifname=lemnos%d`.
pub const BOARD_IF: &str = "lemnos0";
const BOARD_ADDR: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 1);
const HOST_ADDR: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 2);
const PREFIX: u8 = 24;
const PORT: u16 = 7700;
/// How often the running connection sends, and how long it waits for each echo.
const PACE: Duration = Duration::from_millis(50);
const ECHO_WAIT: Duration = Duration::from_secs(10);
/// How long a fresh connect has.
const CONNECT_WAIT: Duration = Duration::from_secs(2);
/// How long the peer has to say each thing.
const PEER_WAIT: Duration = Duration::from_secs(20);

// Routing netlink, from linux/netlink.h, linux/rtnetlink.h, linux/if_link.h and linux/if_addr.h.
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_EXCL: u16 = 0x200;
const NLM_F_CREATE: u16 = 0x400;
const NLMSG_ERROR: u16 = 2;
const RTM_NEWLINK: u16 = 16;
const RTM_NEWADDR: u16 = 20;
const IFLA_NET_NS_FD: u16 = 28;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFF_UP: u32 = 1;
const AF_INET: u8 = 2;

/// The interface index of `name` in this namespace.
pub fn index(name: &str) -> io::Result<u32> {
    lemnos_linux_sys::tcp::interface_index(name)
}

/// The host's interface of the link: the network device the `cdc_ncm` driver made, if there is one.
pub fn host_interface() -> Option<String> {
    fs::read_dir("/sys/class/net")
        .ok()?
        .flatten()
        .find_map(|entry| {
            let driver = fs::read_link(entry.path().join("device/driver")).ok()?;
            (driver.file_name()? == "cdc_ncm")
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
}

/// One netlink request, `kind` with `body`, answered with the kernel's acknowledgement.
fn request(fd: BorrowedFd<'_>, kind: u16, flags: u16, body: &[u8]) -> io::Result<()> {
    let len = 16 + body.len();
    let mut message = Vec::with_capacity(len);
    message.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(&(NLM_F_REQUEST | NLM_F_ACK | flags).to_ne_bytes());
    message.extend_from_slice(&1u32.to_ne_bytes());
    message.extend_from_slice(&0u32.to_ne_bytes());
    message.extend_from_slice(body);
    lemnos_linux_sys::guest::send(fd, &message)?;
    let mut answer = [0u8; 1024];
    let n = lemnos_linux_sys::guest::read(fd, &mut answer)?;
    let field = |at: usize| answer.get(at..at + 4).map(|b| [b[0], b[1], b[2], b[3]]);
    let kind = answer.get(4..6).map(|b| u16::from_ne_bytes([b[0], b[1]]));
    match (n >= 20, kind, field(16).map(i32::from_ne_bytes)) {
        (true, Some(NLMSG_ERROR), Some(0)) => Ok(()),
        (true, Some(NLMSG_ERROR), Some(error)) => Err(io::Error::from_raw_os_error(-error)),
        _ => Err(io::Error::other(
            "the kernel's answer is not an acknowledgement",
        )),
    }
}

/// An attribute: its length and type, the data, padded to 4 bytes.
fn attribute(kind: u16, data: &[u8]) -> Vec<u8> {
    let len = 4 + data.len();
    let mut bytes = Vec::with_capacity(len.next_multiple_of(4));
    bytes.extend_from_slice(&u16::try_from(len).unwrap_or(u16::MAX).to_ne_bytes());
    bytes.extend_from_slice(&kind.to_ne_bytes());
    bytes.extend_from_slice(data);
    bytes.resize(len.next_multiple_of(4), 0);
    bytes
}

/// `struct ifinfomsg` for interface `index`, with `flags` under `change`.
fn link(index: u32, flags: u32, change: u32) -> Vec<u8> {
    let mut body = vec![0u8, 0, 0, 0];
    body.extend_from_slice(&index.to_ne_bytes());
    body.extend_from_slice(&flags.to_ne_bytes());
    body.extend_from_slice(&change.to_ne_bytes());
    body
}

/// Moves interface `name` into the network namespace `ns` (an open `/proc/PID/ns/net`).
fn move_to(name: &str, ns: &File) -> io::Result<()> {
    let socket = lemnos_linux_sys::guest::route_socket()?;
    let fd = u32::try_from(ns.as_raw_fd()).map_err(io::Error::other)?;
    let mut body = link(index(name)?, 0, 0);
    body.extend(attribute(IFLA_NET_NS_FD, &fd.to_ne_bytes()));
    request(socket.as_fd(), RTM_NEWLINK, 0, &body)
}

/// Sets interface `name` up and gives it `addr`/[`PREFIX`], in this namespace.
fn address(name: &str, addr: Ipv4Addr) -> io::Result<()> {
    let socket = lemnos_linux_sys::guest::route_socket()?;
    let at = index(name)?;
    request(socket.as_fd(), RTM_NEWLINK, 0, &link(at, IFF_UP, IFF_UP))?;
    let mut body = vec![AF_INET, PREFIX, 0, 0];
    body.extend_from_slice(&at.to_ne_bytes());
    body.extend(attribute(IFA_LOCAL, &addr.octets()));
    body.extend(attribute(IFA_ADDRESS, &addr.octets()));
    match request(
        socket.as_fd(),
        RTM_NEWADDR,
        NLM_F_CREATE | NLM_F_EXCL,
        &body,
    ) {
        // Given before, by an earlier start of the peer or the check.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

/// The peer's side, run as `netpeer NAME`: takes a network namespace of its own, waits until the
/// check has moved interface `NAME` into it, gives the interface the host's address, and echoes
/// every connection on [`PORT`]. A line `index` on its input asks for `NAME`'s index.
pub fn run_peer(name: &str) -> ! {
    let out = |line: &str| {
        let mut stdout = io::stdout().lock();
        let _ = writeln!(stdout, "netpeer: {line}");
        let _ = stdout.flush();
    };
    if let Err(error) = lemnos_linux_sys::guest::unshare_net() {
        out(&format!("FAIL a network namespace of its own: {error}"));
        std::process::exit(1);
    }
    out("ns");
    let mut lines = io::stdin().lock().lines();
    if !matches!(lines.next(), Some(Ok(line)) if line == "moved") {
        out("FAIL the interface was not moved");
        std::process::exit(1);
    }
    if let Err(error) = address(name, HOST_ADDR) {
        out(&format!("FAIL {name} gets the host's address: {error}"));
        std::process::exit(1);
    }
    let listener = match TcpListener::bind(SocketAddrV4::new(HOST_ADDR, PORT)) {
        Ok(listener) => listener,
        Err(error) => {
            out(&format!("FAIL listen on {HOST_ADDR}:{PORT}: {error}"));
            std::process::exit(1);
        }
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || echo(stream));
        }
    });
    out("listening");
    for line in lines.map_while(Result::ok) {
        if line == "index" {
            match index(name) {
                Ok(at) => out(&format!("index {at}")),
                Err(error) => out(&format!("index none ({error})")),
            }
        }
    }
    std::process::exit(0);
}

fn echo(mut stream: TcpStream) {
    let mut buf = [0u8; 256];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
        }
    }
}

/// The peer, as the check holds it.
pub struct Peer {
    child: Child,
    input: ChildStdin,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Peer {
    /// Starts the peer, moves the host's interface into its namespace, and gives the board's
    /// interface its address.
    pub fn start(me: &Path) -> Result<Self, String> {
        let host = host_interface().ok_or("the host has no cdc_ncm network device")?;
        let mut child = Command::new(me)
            .args(["netpeer", &host])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("start the peer: {error}"))?;
        let input = child.stdin.take().ok_or("the peer has no input")?;
        let stdout = child.stdout.take().ok_or("the peer has no output")?;
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let kept = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                say!("     {line}");
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line);
            }
        });
        let mut peer = Self {
            child,
            input,
            lines,
        };
        peer.said("netpeer: ns")?;
        let ns = File::open(format!("/proc/{}/ns/net", peer.child.id()))
            .map_err(|error| format!("the peer's namespace: {error}"))?;
        move_to(&host, &ns).map_err(|error| format!("move {host} to the peer: {error}"))?;
        writeln!(peer.input, "moved").map_err(|error| format!("tell the peer: {error}"))?;
        peer.said("netpeer: listening")?;
        address(BOARD_IF, BOARD_ADDR)
            .map_err(|error| format!("{BOARD_IF} gets the board's address: {error}"))?;
        say!(
            "     net: {BOARD_IF} {BOARD_ADDR}/{PREFIX}, the host's {host} {HOST_ADDR}/{PREFIX} in the peer's namespace"
        );
        Ok(peer)
    }

    /// Waits until the peer has said `line`, or fails with what it said instead.
    fn said(&mut self, line: &str) -> Result<(), String> {
        let until = Instant::now() + PEER_WAIT;
        loop {
            let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
            if lines.iter().any(|l| l == line) {
                return Ok(());
            }
            if let Some(fail) = lines.iter().find(|l| l.contains("FAIL")) {
                return Err(fail.clone());
            }
            drop(lines);
            if Instant::now() >= until || matches!(self.child.try_wait(), Ok(Some(_))) {
                return Err(format!("the peer never said {line:?}"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The index of the host's interface, in the peer's namespace.
    pub fn host_index(&mut self) -> Option<u32> {
        let before = self.count_index();
        writeln!(self.input, "index").ok()?;
        let until = Instant::now() + PEER_WAIT;
        while Instant::now() < until {
            let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
            let answers: Vec<&String> = lines
                .iter()
                .filter(|l| l.starts_with("netpeer: index "))
                .collect();
            if answers.len() > before {
                return answers
                    .last()
                    .and_then(|l| l.strip_prefix("netpeer: index "))
                    .and_then(|n| n.parse().ok());
            }
            drop(lines);
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    fn count_index(&self) -> usize {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|l| l.starts_with("netpeer: index "))
            .count()
    }
}

fn peer_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(HOST_ADDR, PORT))
}

/// A TCP connection from the board to the host, open for as long as it runs: every [`PACE`] it sends
/// a count and waits for the echo, and keeps when each echo came.
pub struct Stream {
    echoes: Arc<Mutex<Vec<Instant>>>,
    failed: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
}

impl Stream {
    /// The connection to the host over the link.
    pub fn start() -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&peer_addr(), CONNECT_WAIT)
            .map_err(|error| format!("connect to the host over the link: {error}"))?;
        Ok(Self::paced(stream))
    }

    /// The reference: the same paced echo over this namespace's loopback, to an echo thread of
    /// this process. It goes through no USB at all, so a gap it shows as well is the guest's
    /// scheduling (a busy machine under the guest), not the link.
    pub fn loopback() -> Result<Self, String> {
        let at = Ipv4Addr::LOCALHOST;
        let socket = lemnos_linux_sys::guest::route_socket()
            .map_err(|error| format!("the loopback: {error}"))?;
        let lo = index("lo").map_err(|error| format!("the loopback's index: {error}"))?;
        request(socket.as_fd(), RTM_NEWLINK, 0, &link(lo, IFF_UP, IFF_UP))
            .map_err(|error| format!("set the loopback up: {error}"))?;
        let listener = TcpListener::bind(SocketAddrV4::new(at, 0))
            .map_err(|error| format!("listen on the loopback: {error}"))?;
        let addr = listener
            .local_addr()
            .map_err(|error| format!("the loopback's port: {error}"))?;
        std::thread::spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                std::thread::spawn(move || echo(stream));
            }
        });
        let stream = TcpStream::connect_timeout(&addr, CONNECT_WAIT)
            .map_err(|error| format!("connect over the loopback: {error}"))?;
        Ok(Self::paced(stream))
    }

    fn paced(mut stream: TcpStream) -> Self {
        let timeout = stream.set_read_timeout(Some(ECHO_WAIT));
        let echoes: Arc<Mutex<Vec<Instant>>> = Arc::default();
        let failed: Arc<Mutex<Option<String>>> = Arc::default();
        let running = Arc::new(AtomicBool::new(true));
        let (kept, failure, on) = (
            Arc::clone(&echoes),
            Arc::clone(&failed),
            Arc::clone(&running),
        );
        if let Err(error) = timeout {
            *failed.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(format!("the connection's timeout: {error}"));
        }
        std::thread::spawn(move || {
            let mut count = 0u64;
            while on.load(Ordering::Relaxed) {
                count += 1;
                let mut back = [0u8; 8];
                let result = stream
                    .write_all(&count.to_be_bytes())
                    .and_then(|()| stream.read_exact(&mut back));
                match result {
                    Ok(()) if back == count.to_be_bytes() => {
                        kept.lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(Instant::now());
                    }
                    Ok(()) => {
                        *failure.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(format!("echo {count} came back as something else"));
                        return;
                    }
                    Err(error) => {
                        *failure.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(format!("echo {count}: {error}"));
                        return;
                    }
                }
                std::thread::sleep(PACE);
            }
        });
        Self {
            echoes,
            failed,
            running,
        }
    }

    /// Why the connection ended, if it did.
    pub fn failed(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many echoes came after `since`.
    pub fn echoes_since(&self, since: Instant) -> usize {
        self.echoes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|at| **at >= since)
            .count()
    }

    /// The longest time with no echo between `from` and `to`, counting from `from` to the first
    /// echo and from the last to `to`.
    pub fn longest_gap(&self, from: Instant, to: Instant) -> Duration {
        let echoes = self.echoes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut last = from;
        let mut longest = Duration::ZERO;
        for at in echoes.iter().filter(|at| **at >= from && **at <= to) {
            longest = longest.max(at.duration_since(last));
            last = *at;
        }
        longest.max(to.saturating_duration_since(last))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Makes `count` fresh connections to the host, one after another, each sending a line and reading
/// it back. How many did.
pub fn connects(count: usize) -> usize {
    (0..count)
        .filter(|n| {
            let Ok(mut stream) = TcpStream::connect_timeout(&peer_addr(), CONNECT_WAIT) else {
                return false;
            };
            let line = format!("connect {n}\n");
            let mut back = vec![0u8; line.len()];
            stream.set_read_timeout(Some(CONNECT_WAIT)).is_ok()
                && stream.write_all(line.as_bytes()).is_ok()
                && stream.read_exact(&mut back).is_ok()
                && back == line.as_bytes()
        })
        .count()
}
