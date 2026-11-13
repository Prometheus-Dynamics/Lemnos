#![allow(clippy::print_stderr)] // the harness reports its steps on stderr, by design
//! The root side of the holder checks: the host's port controls, its device resets and
//! configurations, its bulk transfers and the FunctionFS mounts. The check's own process has
//! dropped to the daemon's user by then and can do none of these, so this child is started before
//! the drop, as the host is, and keeps its root.
//!
//! `lemnos-gadget-check hub` reads one command per line on standard input and writes one reply
//! per line on standard output: `ok`, or `err N` with the errno N. Commands:
//!
//! - `off` / `on`: the root hub's port power, off disconnects the device and on connects it again.
//! - `suspend` / `resume`: the port's suspend, which the gadget sees as a suspend and a resume.
//! - `reset`: the device's bus reset (`USBDEVFS_RESET`), which the gadget sees as a reset.
//! - `cfg N`: SET_CONFIGURATION N to the device; `cfg 0` leaves the function disabled.
//! - `halt` / `unhalt`: the host halts the bulk OUT endpoint, or clears the halt.
//! - `ctl`: a vendor request to interface 0, device to host, which nothing on the daemon's side serves:
//!   the device stalls it, and the host sees `EPIPE`.
//! - `bulk`: claims interface 0 and moves one chunk out of the bulk OUT endpoint and back on the
//!   IN endpoint, checking the bytes; retried until the device is configured again.
//! - `mount`: mounts the gadget's FunctionFS instance on its mount point.
//! - `umount`: unmounts the mount point; `detach`: unmounts it lazily (`MNT_DETACH`).

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use lemnos_linux_sys::mount;
use lemnos_linux_sys::usbfs::{ControlSetup, UsbfsFile};

use crate::{CHUNK, EP_IN, EP_OUT, MOUNTS, UDC, find_device};

/// The root hub: the first bus, its device one.
const ROOT_HUB: &str = "/sys/bus/usb/devices/usb1";
/// The root hub's only port.
const PORT: u16 = 1;
const PORT_POWER: u16 = 8;
const PORT_SUSPEND: u16 = 2;
const SET_FEATURE: u8 = 3;
const CLEAR_FEATURE: u8 = 1;
/// Host to device, class, other: a request to a port.
const TO_PORT: u8 = 0x23;
const SET_CONFIGURATION: u8 = 9;
const EINVAL: i32 = 22;
const EIO: i32 = 5;
const ENODEV: i32 = 19;
const TIMEOUT_MS: u32 = 10_000;
/// How long `bulk` keeps trying: a device that was just connected or configured needs a moment.
const BULK_FOR: Duration = Duration::from_secs(20);

/// Runs the command loop until standard input closes.
pub fn run() {
    let mut output = io::stdout();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let reply = match command(line.trim()) {
            Ok(()) => "ok".to_string(),
            Err(error) => format!("err {}", error.raw_os_error().unwrap_or(EIO)),
        };
        if writeln!(output, "{reply}")
            .and_then(|()| output.flush())
            .is_err()
        {
            break;
        }
    }
}

fn command(line: &str) -> io::Result<()> {
    let mut words = line.split_whitespace();
    let target = || Path::new(MOUNTS).join(UDC);
    match words.next() {
        Some("off") => port(CLEAR_FEATURE, PORT_POWER),
        Some("on") => port(SET_FEATURE, PORT_POWER),
        Some("suspend") => port(SET_FEATURE, PORT_SUSPEND),
        Some("resume") => {
            let result = port(CLEAR_FEATURE, PORT_SUSPEND);
            // The controller finishes a resume only when the root hub is polled, so poll the port
            // until its suspend bit clears (or a second passes), and say what it showed.
            let until = Instant::now() + Duration::from_secs(1);
            let mut status = port_status();
            while matches!(status, Ok((word, _)) if word & 0x0004 != 0) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
                status = port_status();
            }
            eprintln!("hub: port status after resume: {status:?}");
            result
        }
        Some("reset") => UsbfsFile::open(&device()?)?.reset(),
        Some("cfg") => {
            let value: u16 = words
                .next()
                .and_then(|word| word.parse().ok())
                .ok_or_else(|| io::Error::from_raw_os_error(EINVAL))?;
            let setup = ControlSetup {
                request_type: 0x00,
                request: SET_CONFIGURATION,
                value,
                index: 0,
            };
            UsbfsFile::open(&device()?)?
                .control_out(setup, &[], TIMEOUT_MS)
                .map(drop)
        }
        Some("halt") => endpoint_feature(SET_FEATURE, EP_OUT),
        Some("unhalt") => endpoint_feature(CLEAR_FEATURE, EP_OUT),
        Some("ctl") => request_interface(),
        Some("bulk") => bulk(),
        Some("mount") => mount::functionfs(UDC, &target(), ""),
        Some("umount") => mount::unmount(&target()),
        Some("detach") => mount::detach(&target()),
        _ => Err(io::Error::from_raw_os_error(EINVAL)),
    }
}

/// A SetFeature or ClearFeature of ENDPOINT_HALT on `endpoint`: the host halts or clears it.
fn endpoint_feature(request: u8, endpoint: u8) -> io::Result<()> {
    let setup = ControlSetup {
        request_type: 0x02,
        request,
        value: 0,
        index: u16::from(endpoint),
    };
    UsbfsFile::open(&device()?)?
        .control_out(setup, &[], TIMEOUT_MS)
        .map(drop)
}

/// A SetPortFeature or ClearPortFeature on the root hub's port.
fn port(request: u8, feature: u16) -> io::Result<()> {
    let root = UsbfsFile::open(&root_hub()?)?;
    let setup = ControlSetup {
        request_type: TO_PORT,
        request,
        value: feature,
        index: PORT,
    };
    root.control_out(setup, &[], TIMEOUT_MS).map(drop)
}

/// The root port's status word and change word (GetPortStatus), for a diagnosis on the console.
fn port_status() -> io::Result<(u16, u16)> {
    let root = UsbfsFile::open(&root_hub()?)?;
    let setup = ControlSetup {
        request_type: 0xa3,
        request: 0,
        value: 0,
        index: PORT,
    };
    let mut buf = [0u8; 4];
    root.control_in(setup, &mut buf, TIMEOUT_MS)?;
    Ok((
        u16::from_le_bytes([buf[0], buf[1]]),
        u16::from_le_bytes([buf[2], buf[3]]),
    ))
}

/// The root hub's usbfs node, `/dev/bus/usb/BBB/DDD`.
fn root_hub() -> io::Result<PathBuf> {
    let read = |name: &str| -> io::Result<u32> {
        std::fs::read_to_string(Path::new(ROOT_HUB).join(name))?
            .trim()
            .parse()
            .map_err(|_| io::Error::from_raw_os_error(EIO))
    };
    let (bus, dev) = (read("busnum")?, read("devnum")?);
    Ok(PathBuf::from(format!("/dev/bus/usb/{bus:03}/{dev:03}")))
}

/// The gadget's usbfs node as the host sees it now.
fn device() -> io::Result<PathBuf> {
    find_device(Duration::from_secs(10)).ok_or_else(|| io::Error::from_raw_os_error(ENODEV))
}

/// A vendor request to interface 0, device to host: stalled by the device (`EPIPE` here) when no
/// consumer on the daemon's side answers it.
fn request_interface() -> io::Result<()> {
    let setup = ControlSetup {
        request_type: 0xc1,
        request: 0x42,
        value: 0,
        index: 0,
    };
    let mut buf = [0u8; 8];
    UsbfsFile::open(&device()?)?
        .control_in(setup, &mut buf, TIMEOUT_MS)
        .map(drop)
}

/// One chunk out of the bulk OUT endpoint and back on the IN endpoint, checked, retried for
/// `BULK_FOR` while the device comes back.
fn bulk() -> io::Result<()> {
    let until = Instant::now() + BULK_FOR;
    let mut pattern = [0u8; CHUNK];
    for (i, byte) in pattern.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    loop {
        match bulk_once(&pattern) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= until => return Err(error),
            Err(error) => {
                eprintln!("hub: bulk attempt failed: {error}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn bulk_once(pattern: &[u8; CHUNK]) -> io::Result<()> {
    let usb = UsbfsFile::open(&device()?)?;
    usb.claim_interface(0)?;
    let sent = usb.bulk_out(EP_OUT, pattern, TIMEOUT_MS)?;
    let mut back = [0u8; CHUNK];
    let got = usb.bulk_in(EP_IN, &mut back, TIMEOUT_MS)?;
    usb.release_interface(0)?;
    if sent != CHUNK || got != CHUNK || back != *pattern {
        return Err(io::Error::from_raw_os_error(EIO));
    }
    Ok(())
}

/// The check's end of the root child: one command, then its reply.
pub struct Hub {
    _child: Child,
    to: ChildStdin,
    from: BufReader<ChildStdout>,
}

impl Hub {
    /// Starts the root child from the same program. Call before the daemon's privileged start, so
    /// the child is still root.
    pub fn start(program: &Path) -> io::Result<Self> {
        let mut child = Command::new(program)
            .arg("hub")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let to = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::from_raw_os_error(EIO))?;
        let from = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| io::Error::from_raw_os_error(EIO))?,
        );
        Ok(Self {
            _child: child,
            to,
            from,
        })
    }

    /// Sends `command` and waits for its reply: `Ok` for `ok`, the errno for `err N`.
    pub fn ask(&mut self, command: &str) -> io::Result<()> {
        writeln!(self.to, "{command}")?;
        self.to.flush()?;
        let mut reply = String::new();
        if self.from.read_line(&mut reply)? == 0 {
            return Err(io::Error::from_raw_os_error(EIO));
        }
        match reply.trim() {
            "ok" => Ok(()),
            other => {
                let code = other
                    .strip_prefix("err ")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(EIO);
                // Named on the console, so a failed step says which command it was.
                eprintln!("hub: {command:?} failed: {other:?}");
                Err(io::Error::from_raw_os_error(code))
            }
        }
    }
}
