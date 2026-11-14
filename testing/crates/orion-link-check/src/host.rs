//! The host side over the emulated USB bus: the role `host`, a child process of the check. It finds
//! the gadget on the bus, claims the link's interface and carries the link's bytes between the bulk
//! endpoints and a socket pair, and a real Orion client session runs over the socket: the handshake
//! (hello and welcome), then ping and pong round trips with the node's comm core.
//!
//! The bytes between the endpoints and the session are not interpreted, only watched: a tap on the
//! IN direction decodes the frames that pass so each pong can be counted (the session answers pings
//! and swallows pongs itself, so it cannot say how many came).

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lemnos_linux_sys::usbfs::UsbfsFile;
use orion_connect::{Options, Roles, Session};
use orion_msg_session::{Ping, Pong};
use orion_wire::{Decode, StreamDecoder, kind};

use crate::{EP_IN, EP_OUT, LINK_INTERFACE};

/// The most one bulk transfer carries in the shim.
const TRANSFER: usize = 16 * 1024;
/// The first number the pings carry.
const BASE: u64 = 7_000_000;

/// The pongs the tap has seen, by the number their ping carried.
#[derive(Default)]
struct Pongs {
    seen: Mutex<Vec<u64>>,
}

/// The host side. `true` when the session came up and every round trip came back.
pub fn run(rounds: u64) -> bool {
    let Some(path) = find_device(Duration::from_secs(60)) else {
        say!("FAIL the host sees the gadget on the bus");
        return false;
    };
    let usb = match UsbfsFile::open(&path) {
        Ok(usb) => Arc::new(usb),
        Err(error) => {
            say!("FAIL open {}: {error}", path.display());
            return false;
        }
    };
    if let Err(error) = usb.claim_interface(LINK_INTERFACE) {
        say!("FAIL claim the link's interface: {error}");
        return false;
    }
    say!(
        "ok   host: the link's interface is claimed on {}",
        path.display()
    );

    let (client, shim) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(error) => {
            say!("FAIL host: socket pair: {error}");
            return false;
        }
    };
    let pongs = Arc::new(Pongs::default());
    // The device to the session, through the tap.
    {
        let usb = Arc::clone(&usb);
        let pongs = Arc::clone(&pongs);
        let mut to_session = match shim.try_clone() {
            Ok(stream) => stream,
            Err(error) => {
                say!("FAIL host: socket: {error}");
                return false;
            }
        };
        std::thread::spawn(move || {
            let mut tap = Tap::new();
            let mut buffer = vec![0u8; TRANSFER];
            while let Ok(len) = usb.bulk_in(EP_IN, &mut buffer, 0) {
                tap.watch(&buffer[..len], &pongs);
                if std::io::Write::write_all(&mut to_session, &buffer[..len]).is_err() {
                    break;
                }
            }
        });
    }
    // The session to the device.
    {
        let usb = Arc::clone(&usb);
        let mut from_session = shim;
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; TRANSFER];
            while let Ok(len) = std::io::Read::read(&mut from_session, &mut buffer) {
                if len == 0 || usb.bulk_out(EP_OUT, &buffer[..len], 30_000).is_err() {
                    break;
                }
            }
        });
    }

    let options = Options {
        roles: Roles::DEVICE,
        timeout: Duration::from_secs(30),
        ..Options::default()
    };
    let mut session = match Session::over_link(OwnedFd::from(client), "usb-host", options) {
        Ok(session) => session,
        Err(error) => {
            say!("FAIL host: the session's handshake: {error:?}");
            return false;
        }
    };
    let welcomed = session.welcomed().clone();
    say!(
        "ok   host session up: welcomed by node {:?}, session {}, heartbeat {:?}",
        welcomed.node.to_string(),
        welcomed.session,
        welcomed.heartbeat
    );

    for round in 0..rounds {
        let now_ms = BASE + round;
        if let Err(error) = session.send(&Ping { now_ms }) {
            say!("FAIL host: ping {round} not sent: {error:?}");
            return false;
        }
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            // The session reads the link too: it answers the node's pings and keeps the heartbeat.
            if let Err(error) = session.next(Duration::from_millis(10)) {
                say!("FAIL host: the session failed at round {round}: {error:?}");
                return false;
            }
            if pongs
                .seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&now_ms)
            {
                break;
            }
            if Instant::now() >= until {
                say!("FAIL host: no pong for ping {round}");
                return false;
            }
        }
    }
    say!("ok   host: {rounds} ping and pong round trips with the node");
    true
}

/// Decodes the frames of the IN direction as they pass, and notes the pongs.
struct Tap {
    decoder: Box<StreamDecoder<1024>>,
}

impl Tap {
    fn new() -> Self {
        Self {
            decoder: Box::new(StreamDecoder::new()),
        }
    }

    fn watch(&mut self, bytes: &[u8], pongs: &Pongs) {
        let mut at = 0;
        while at < bytes.len() {
            let (used, pushed) = self.decoder.push_slice(&bytes[at..]);
            if used == 0 {
                break;
            }
            at += used;
            if let Ok(Some(frame)) = pushed
                && frame.kind() == kind::PONG
                && let Ok(pong) = Pong::decode(frame.payload())
            {
                pongs
                    .seen
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pong.now_ms);
            }
        }
    }
}

/// The gadget on the bus by the identity `contracts/usb-link.toml` gives.
fn find_device(within: Duration) -> Option<PathBuf> {
    let (vendor, product) = crate::identity();
    let until = Instant::now() + within;
    loop {
        if let Some(path) = scan(&format!("{vendor:04x}"), &format!("{product:04x}")) {
            return Some(path);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn scan(vendor: &str, product: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir("/sys/bus/usb/devices").ok()?.flatten() {
        let dir = entry.path();
        let text = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_string())
        };
        if text("idVendor").as_deref() != Some(vendor)
            || text("idProduct").as_deref() != Some(product)
        {
            continue;
        }
        let bus: u32 = text("busnum")?.parse().ok()?;
        let dev: u32 = text("devnum")?.parse().ok()?;
        let path = PathBuf::from(format!("/dev/bus/usb/{bus:03}/{dev:03}"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}
