#![allow(clippy::print_stderr)] // the harness reports its steps on stderr, by design
//! The holder checks: what a process that holds the gadget's bulk pair sees when the host or the
//! daemon changes the function underneath it, and whether END (the daemon's word that a hold is
//! over for good) hangs up when it should.
//!
//! The holder is a client of the daemon (`lemnos-client`, [`Hold::take`]): it takes the pair and
//! END with `usb_endpoints_end`, as Orion does, and keeps its copies of the descriptors. Each case
//! has the same shape. Before the event, a read is waiting on the configured host (a), and a queued
//! AIO read (b) is under way (the backend's own form). The event comes next. Then the errno of each
//! read in flight is checked, and a new read (d) and a new write (c) made after the event are checked
//! too, before the same descriptors carry a chunk again.
//!
//! The events come from [`Hub`] (the root side: port power, suspend, reset, configuration, halt,
//! control requests, mount), from the daemon's own process (killed and started again, case 6), its
//! gadget's configfs attribute (the UDC unbind, case 4), and the client's power switch (case 5).

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_client::{Error, Session};
use lemnos_core::Status;
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::aio::Queue;

use crate::daemon::{self, Lemnosd};
use crate::hub::Hub;
use crate::{CHUNK, GADGET_DIR};

/// `ESHUTDOWN`: a request in flight when the endpoint was disabled.
const ESHUTDOWN: i32 = 108;
/// `EAGAIN`: a request on a disabled endpoint, on a descriptor that is non-blocking.
const EAGAIN: i32 = 11;
/// `EBUSY`: a mount or unmount while the instance is still in use.
const EBUSY: i32 = 16;
/// `EPIPE`: a control request the device stalls.
const EPIPE: i32 = 32;
/// How long a read is expected to still be waiting.
const STILL: Duration = Duration::from_millis(500);
/// How long a read that should answer at once may take.
const SHORT: Duration = Duration::from_secs(3);
/// The longest any step waits for the host or the daemon.
const LONG: Duration = Duration::from_secs(20);
/// The contract the daemon lends the bulk pair through.
const USB_FUNCTION: &str = "usb.function/1";

/// The bulk pair and END, taken from the daemon through a client connection that holds them for as
/// long as it lives. END's answer is kept once it has come.
pub struct Hold {
    /// The connection the hold belongs to: closing it ends the hold.
    pub session: Session,
    /// The device index the daemon knows the function by.
    pub device: u32,
    /// The OUT endpoint, as the holder's copy.
    pub out: OwnedFd,
    /// The IN endpoint, as the holder's copy.
    pub input: OwnedFd,
    end: Receiver<io::Result<usize>>,
    end_answer: Option<io::Result<usize>>,
}

impl Hold {
    /// Takes the pair and END from `daemon`. Asks until the function is enabled (`NO_DEVICE`) and
    /// nobody else holds it (`BUSY`) for at most [`LONG`].
    pub fn take(daemon: &Lemnosd) -> io::Result<Self> {
        let mut session = daemon.connect().map_err(fail)?;
        let device = session
            .find(USB_FUNCTION)
            .map_err(fail)?
            .ok_or_else(|| io::Error::other("the daemon lists no usb.function device"))?
            .index;
        let until = Instant::now() + LONG;
        let lent = loop {
            match session.usb_endpoints_end(device, Arc::new(LinuxStreams)) {
                Ok(lent) => break lent,
                Err(Error::Refused { status, .. })
                    if matches!(status, Status::NO_DEVICE | Status::BUSY)
                        && Instant::now() < until =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(fail(error)),
            }
        };
        Ok(Self {
            session,
            device,
            out: lent.out,
            input: lent.input,
            end: watch_end(lent.end),
            end_answer: None,
        })
    }

    /// END's answer, asked for within `within` and kept once it came.
    fn end_within(&mut self, within: Duration) -> Option<&io::Result<usize>> {
        if self.end_answer.is_none() {
            self.end_answer = self.end.recv_timeout(within).ok();
        }
        self.end_answer.as_ref()
    }

    /// Whether END is still open after `within`: no hang-up came.
    fn end_open(&mut self, within: Duration) -> bool {
        self.end_within(within).is_none()
    }

    /// Whether END hangs up (reads EOF) within `within`.
    fn end_hung_up(&mut self, within: Duration) -> bool {
        matches!(self.end_within(within), Some(Ok(0)))
    }
}

/// Reads END on a thread, so the cases can ask whether it has hung up without blocking.
fn watch_end(end: OwnedFd) -> Receiver<io::Result<usize>> {
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let mut byte = [0u8; 1];
        let answer = File::from(end).read(&mut byte);
        let _ = sender.send(answer);
    });
    receiver
}

/// A driver or daemon error as an `io::Error`, for the holder checks to report.
fn fail<E: std::fmt::Debug>(error: E) -> io::Error {
    io::Error::other(format!("{error:?}"))
}

/// Runs every case. `check` gets each assertion's result and text, and returns it. The hold is
/// taken by value: the cases end it (the device is switched off in case 5).
pub fn run(
    daemon: &mut Lemnosd,
    hub: &mut Hub,
    hold: Hold,
    check: &mut dyn FnMut(bool, &str) -> bool,
) {
    if let Err(error) = cases(daemon, hub, hold, &mut *check) {
        check(false, &format!("the holder checks ran to the end: {error}"));
    }
}

/// One round of the holder's echo: a chunk from OUT written back on IN. Whether it went through.
pub fn echo_round(hold: &Hold) -> bool {
    spawn_echo(&hold.out, &hold.input).is_ok_and(|echo| echoed(&echo))
}

fn cases(
    daemon: &mut Lemnosd,
    hub: &mut Hub,
    mut hold: Hold,
    check: &mut dyn FnMut(bool, &str) -> bool,
) -> io::Result<()> {
    // 1. The host disconnects (port power off) and connects again (power on). A sync read, then
    // a queued AIO read on its own: a second reader on the same endpoint would share its request.
    let waiting = read_once(&hold.out)?;
    check(still(&waiting), "1 (a) a read on a configured host waits");
    hub.ask("off")?;
    let inflight = errno(waiting.recv_timeout(LONG).ok());
    check(
        inflight == Some(ESHUTDOWN),
        &format!("1 (a) the read in flight at a disconnect fails ESHUTDOWN (got {inflight:?})"),
    );
    check(
        hold.end_open(STILL),
        "1 END stays open while the host is away",
    );
    let fresh = errno(read_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!("1 (d) a new read while the host is away fails EAGAIN at once (got {fresh:?})"),
    );
    let written = errno(write_once(&hold.input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("1 (c) a write while the host is away fails EAGAIN (got {written:?})"),
    );
    hub.ask("on")?;
    let echo = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "1 after the host connects again, the same descriptors carry the bytes",
    );
    let queued = aio_once(&hold.out)?;
    check(
        still_aio(&queued),
        "1 (b) a queued AIO read on a configured host waits",
    );
    hub.ask("off")?;
    let queued_err = errno(queued.recv_timeout(LONG).ok());
    check(
        queued_err == Some(ESHUTDOWN),
        &format!("1 (b) the queued AIO read at a disconnect fails ESHUTDOWN (got {queued_err:?})"),
    );
    let fresh_aio = errno(aio_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("1 (b) a new AIO read while the host is away fails EAGAIN (got {fresh_aio:?})"),
    );
    hub.ask("on")?;
    let echo = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "1 (b) after the host connects again the same descriptors carry the bytes",
    );
    check(
        hold.end_open(STILL),
        "1 END stays open after the host connects again",
    );

    // 2a. The host suspends and resumes; the read waiting through it takes the bytes, and a new read
    // after the resume does too.
    let echo = spawn_echo(&hold.out, &hold.input)?;
    check(
        still(&echo),
        "2a (a) the read keeps waiting through a suspend: no code",
    );
    hub.ask("suspend")?;
    check(still(&echo), "2a (a) the read still waits while suspended");
    hub.ask("resume")?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "2a after a resume the waiting read takes the host's bytes and the write answers",
    );
    let echo = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "2a (d) a new read after the resume takes the host's bytes and the write answers",
    );
    check(hold.end_open(STILL), "2a END stays open through a suspend");

    // 2b. The host resets the bus: the read in flight fails, and a read issued just after the reset
    // either fails EAGAIN (the host has not configured yet) or waits, and then takes the host's
    // chunk and writes it back itself, so the transfer completes.
    let echo = spawn_echo(&hold.out, &hold.input)?;
    check(
        still(&echo),
        "2b (a) the read is waiting again before the bus reset",
    );
    hub.ask("reset")?;
    let (read, _) = echo.recv_timeout(LONG).map_err(io::Error::other)?;
    let reset = read.err().and_then(|e| e.raw_os_error());
    check(
        reset == Some(ESHUTDOWN),
        &format!("2b (a) the read in flight at a bus reset fails ESHUTDOWN (got {reset:?})"),
    );
    let after_reset = read_once(&hold.out)?;
    let after_now = after_reset.recv_timeout(SHORT);
    let after_label = match &after_now {
        Ok(Err(error)) => format!("fails {:?}", error.raw_os_error()),
        Ok(Ok(bytes)) => format!("returns {} bytes", bytes.len()),
        Err(_) => "waits".to_string(),
    };
    let after_ok = !matches!(&after_now, Ok(Err(error)) if error.raw_os_error() != Some(EAGAIN));
    check(
        after_ok,
        &format!("2b (d) a new read just after the reset fails EAGAIN or waits ({after_label})"),
    );
    if after_now.is_err() {
        let (sent, taken) = channel();
        let mut writer = File::from(hold.input.try_clone()?);
        thread::spawn(move || {
            let count = match after_reset.recv_timeout(LONG) {
                Ok(Ok(bytes)) => {
                    let _ = writer.write_all(&bytes);
                    bytes.len()
                }
                _ => 0,
            };
            let _ = sent.send(count);
        });
        hub.ask("bulk")?;
        let count = taken.recv_timeout(LONG).unwrap_or(0);
        check(
            count == CHUNK,
            &format!(
                "2b (d) the read that waited takes the host's chunk and writes it back ({count} bytes)"
            ),
        );
    }
    let echo = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "2b after the reset and the host's configuration, the same descriptors carry the bytes",
    );
    check(
        hold.end_open(STILL),
        "2b END stays open through a bus reset",
    );

    // 3. The host sets configuration 0 (the function is disabled), then 1 (enabled again).
    let echo = spawn_echo(&hold.out, &hold.input)?;
    check(
        still(&echo),
        "3 (a) the read is waiting before the configuration change",
    );
    hub.ask("cfg 0")?;
    let (read, _) = echo.recv_timeout(LONG).map_err(io::Error::other)?;
    let unset = read.err().and_then(|e| e.raw_os_error());
    check(
        unset == Some(ESHUTDOWN),
        &format!("3 (a) the read in flight at configuration 0 fails ESHUTDOWN (got {unset:?})"),
    );
    let fresh = errno(read_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!("3 (d) a new read while unconfigured fails EAGAIN (got {fresh:?})"),
    );
    let fresh_aio = errno(aio_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("3 (b) a new AIO read while unconfigured fails EAGAIN (got {fresh_aio:?})"),
    );
    let written = errno(write_once(&hold.input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("3 (c) a write while unconfigured fails EAGAIN (got {written:?})"),
    );
    let echo = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("cfg 1")?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "3 after configuration 1 the same descriptors carry the bytes",
    );
    check(
        hold.end_open(STILL),
        "3 END stays open through configuration 0",
    );

    // 4. The gadget is unbound from its controller (the UDC attribute, written here) and the daemon
    // binds it again: the daemon's gadget driver sees the controller let go, takes the function down
    // and enables it again after its backoff. The holder's copies stay the same descriptors.
    let echo = spawn_echo(&hold.out, &hold.input)?;
    check(still(&echo), "4 (a) the read is waiting before the unbind");
    fs::write(format!("{GADGET_DIR}/UDC"), "\n")?;
    let (read, _) = echo.recv_timeout(LONG).map_err(io::Error::other)?;
    let unbound = read.err().and_then(|e| e.raw_os_error());
    check(
        unbound == Some(ESHUTDOWN),
        &format!("4 (a) the read in flight at the unbind fails ESHUTDOWN (got {unbound:?})"),
    );
    check(
        hold.end_open(STILL),
        "4 END stays open while the gadget is unbound",
    );
    let fresh = errno(read_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!("4 (d) a new read while unbound fails EAGAIN, not ENODEV (got {fresh:?})"),
    );
    let written = errno(write_once(&hold.input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("4 (c) a write while unbound fails EAGAIN (got {written:?})"),
    );
    let rebound = daemon::wait_until(LONG, daemon::enabled);
    if !rebound {
        diagnose(&mut hold);
    }
    check(
        rebound,
        "4 the daemon binds the gadget again after the unbind",
    );
    let echo = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "4 after the daemon's rebind and the host's configuration, the same descriptors carry the bytes",
    );
    let queued = aio_once(&hold.out)?;
    check(
        still_aio(&queued),
        "4 (b) the queued AIO read is waiting before the unbind",
    );
    fs::write(format!("{GADGET_DIR}/UDC"), "\n")?;
    let queued_err = errno(queued.recv_timeout(LONG).ok());
    check(
        queued_err == Some(ESHUTDOWN),
        &format!("4 (b) the queued AIO read at the unbind fails ESHUTDOWN (got {queued_err:?})"),
    );
    let fresh_aio = errno(aio_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("4 (b) a new AIO read while unbound fails EAGAIN (got {fresh_aio:?})"),
    );
    check(
        daemon::wait_until(LONG, daemon::enabled),
        "4 the daemon binds the gadget again after the second unbind",
    );
    let echo = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "4 (b) after the rebind the same descriptors carry the bytes",
    );
    check(hold.end_open(STILL), "4 END stays open through the unbind");

    // Halt: the host halts the bulk OUT endpoint. A read waiting on it keeps waiting (no code),
    // and it takes the bytes once the host clears the halt.
    let echo = spawn_echo(&hold.out, &hold.input)?;
    check(still(&echo), "halt (a) the read is waiting before the halt");
    hub.ask("halt")?;
    check(
        still(&echo),
        "halt (a) the read keeps waiting while the endpoint is halted: no code",
    );
    hub.ask("unhalt")?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "halt after the halt is cleared the waiting read takes the host's bytes",
    );

    // A control request on ep0 while the pair is held. The board has no data stream, so no consumer
    // serves ep0: the daemon stalls the request at once, and the pair sees no code.
    let echo = spawn_echo(&hold.out, &hold.input)?;
    check(
        still(&echo),
        "ctl (a) the read is waiting before the control request",
    );
    let stalled = hub.ask("ctl").err().and_then(|e| e.raw_os_error());
    check(
        stalled == Some(EPIPE),
        &format!(
            "ctl the control request is stalled at once, the host sees EPIPE (got {stalled:?})"
        ),
    );
    check(
        still(&echo),
        "ctl the pair's read keeps waiting: the control request is no code on the pair",
    );
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "ctl after the control request the same descriptors carry the bytes",
    );

    // 6. The daemon's process dies with the pair held. END hangs up at once. The endpoints stay
    // enabled while the holder's copies are open (the host still sees the device): a read in flight
    // keeps waiting until the host moves bytes, and then takes them. A new daemon on the same mount
    // cannot bind the gadget while those copies are open; once the holder closes them, it lends the
    // pair again (the reuse rule: a mount with ep0 is reused, never remounted).
    let waiting = spawn_echo(&hold.out, &hold.input)?;
    check(still(&waiting), "6 (a) a read on a configured host waits");
    daemon.kill();
    check(
        hold.end_hung_up(LONG),
        "6 END hangs up when the daemon's process ends",
    );
    check(
        still(&waiting),
        "6 (a) the read in flight at the daemon's death keeps waiting: the endpoints stay enabled while the holder's copies are open",
    );
    hub.ask("bulk")?;
    check(
        echoed(&waiting),
        "6 (a) after the daemon's death the read takes the host's bytes and writes them back",
    );
    let after = spawn_echo(&hold.out, &hold.input)?;
    check(
        still(&after),
        "6 (d) a new read after the daemon's death waits",
    );
    hub.ask("bulk")?;
    check(
        echoed(&after),
        "6 (d) after the daemon's death a new read takes the host's bytes",
    );
    daemon.restart().map_err(io::Error::other)?;
    std::thread::sleep(Duration::from_secs(2));
    check(
        !gadget_ok(daemon),
        "6 a new daemon does not bind the gadget while the holder keeps its copies",
    );
    drop(hold);
    hold = Hold::take(daemon)?;
    check(
        true,
        "6 once the holder closes its copies, the new daemon lends the pair and END again",
    );
    // The host enumerates the function again after the new daemon binds it: a round trip first, so
    // the next cases start with the host configured.
    let warm = spawn_echo(&hold.out, &hold.input)?;
    hub.ask("bulk")?;
    check(
        echoed(&warm),
        "6 the new pair carries a chunk once the host has configured again",
    );

    // 5. The device is switched off through the daemon (its client's power switch): the daemon
    // disables the function, closes its endpoints, and lets go. END hangs up, the holder's copies
    // stay open. The queued AIO read is the one in flight at the switch (a sync read and an AIO read
    // would share the endpoint's request, and the switch is done once per run).
    let queued = aio_once(&hold.out)?;
    check(
        still_aio(&queued),
        "5 (b) the queued AIO read is waiting before the switch-off",
    );
    hold.session.set_power(hold.device, false).map_err(fail)?;
    let queued_err = errno(queued.recv_timeout(LONG).ok());
    check(
        queued_err == Some(ESHUTDOWN),
        &format!(
            "5 (b) the queued AIO read at the switch-off fails ESHUTDOWN (got {queued_err:?})"
        ),
    );
    check(
        hold.end_hung_up(LONG),
        "5 END hangs up when the device is switched off through the daemon",
    );
    let umounted = hub.ask("umount");
    check(
        umounted.as_ref().err().and_then(|e| e.raw_os_error()) == Some(EBUSY),
        &format!("5 unmount with the holder's files open is EBUSY (got {umounted:?})"),
    );
    let fresh = errno(read_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!(
            "5 (d) a new read after the switch-off fails EAGAIN, descriptors live (got {fresh:?})"
        ),
    );
    let fresh_aio = errno(aio_once(&hold.out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("5 (b) a new AIO read after the switch-off fails EAGAIN (got {fresh_aio:?})"),
    );
    let written = errno(write_once(&hold.input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("5 (c) a write after the switch-off fails EAGAIN (got {written:?})"),
    );
    hub.ask("detach")?;
    let remount = hub.ask("mount");
    check(
        remount.as_ref().err().and_then(|e| e.raw_os_error()) == Some(EBUSY),
        &format!(
            "5 a new offer (mount) while the holder keeps the pair is EBUSY (got {remount:?})"
        ),
    );
    drop(hold.out);
    drop(hold.input);
    let until = Instant::now() + LONG;
    let remounted = loop {
        match hub.ask("mount") {
            Ok(()) => break Ok(()),
            Err(_) if Instant::now() < until => {
                thread::sleep(Duration::from_millis(200));
            }
            Err(error) => break Err(error),
        }
    };
    check(
        remounted.is_ok(),
        &format!(
            "5 after the holder closes its copies, a new offer (mount) works (got {remounted:?})"
        ),
    );
    Ok(())
}

/// What the daemon and the kernel say when the gadget does not come back: the gadget's health and
/// reason, the UDC attribute, and what the controller and the FunctionFS mount list.
fn diagnose(hold: &mut Hold) {
    match hold.session.list() {
        Ok(list) => {
            for entry in list.iter().filter(|e| e.name == "gadget") {
                eprintln!(
                    "holder: gadget state {:?}, health {:?}, reason {:?}",
                    entry.state, entry.health, entry.reason
                );
            }
        }
        Err(error) => eprintln!("holder: the daemon lists no devices: {error:?}"),
    }
    eprintln!(
        "holder: UDC attribute {:?}",
        fs::read_to_string(format!("{GADGET_DIR}/UDC"))
    );
    for dir in ["/sys/class/udc", "/dev/usb-ffs/dummy_udc.0", "/dev/usb-ffs"] {
        let names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        eprintln!("holder: {dir}: {names:?}");
    }
}

/// Whether the daemon reports its gadget healthy, asked through a fresh connection.
fn gadget_ok(daemon: &Lemnosd) -> bool {
    daemon
        .connect()
        .ok()
        .and_then(|mut session| session.list().ok())
        .and_then(|list| list.into_iter().find(|e| e.name == "gadget"))
        .is_some_and(|e| e.health == lemnos_client::DeviceHealth::Ok)
}

/// Reads of one chunk from a copy of `fd`, on its own thread, so the caller can wait on it with a
/// timeout. The answer comes once: the data, or the errno.
fn read_once(fd: &OwnedFd) -> io::Result<Receiver<io::Result<Vec<u8>>>> {
    let mut file = File::from(fd.try_clone()?);
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let mut buffer = vec![0u8; CHUNK];
        let answer = file.read(&mut buffer).map(|n| {
            buffer.truncate(n);
            buffer
        });
        let _ = sender.send(answer);
    });
    Ok(receiver)
}

/// One queued AIO read of a chunk from a copy of `fd`, the backend's own form: the transfer is
/// with the kernel while it waits, and its answer comes once: the data, or the errno. A timeout
/// inside the queue is not an answer; the thread waits again.
fn aio_once(fd: &OwnedFd) -> io::Result<Receiver<io::Result<Vec<u8>>>> {
    let file = fd.try_clone()?;
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let answer = (|| -> io::Result<Vec<u8>> {
            let mut queue = Queue::new(CHUNK)?;
            let mut buffer = vec![0u8; CHUNK];
            loop {
                match queue.read(file.as_fd(), &mut buffer, LONG) {
                    Ok(n) => {
                        buffer.truncate(n);
                        return Ok(buffer);
                    }
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
                    Err(error) => return Err(error),
                }
            }
        })();
        let _ = sender.send(answer);
    });
    Ok(receiver)
}

/// One write of a chunk to a copy of `fd`, on its own thread, the answer its byte count or errno.
fn write_once(fd: &OwnedFd) -> io::Result<Receiver<io::Result<Vec<u8>>>> {
    let mut file = File::from(fd.try_clone()?);
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let data = vec![0x5au8; CHUNK];
        let answer = file.write(&data).map(|n| vec![0u8; n]);
        let _ = sender.send(answer);
    });
    Ok(receiver)
}

/// The holder's loop, once: a read of a chunk from OUT and the same bytes written to IN. An
/// `EAGAIN` while the host is away is waited out, as a holder does; other errors end it.
fn spawn_echo(
    out: &OwnedFd,
    input: &OwnedFd,
) -> io::Result<Receiver<(io::Result<usize>, io::Result<usize>)>> {
    let (sender, receiver) = channel();
    let mut reader = File::from(out.try_clone()?);
    let mut writer = File::from(input.try_clone()?);
    thread::spawn(move || {
        let until = Instant::now() + LONG;
        let mut buffer = vec![0u8; CHUNK];
        let read = retry(until, || reader.read(&mut buffer));
        let write = match &read {
            Ok(n) => retry(until, || writer.write(&buffer[..*n])),
            Err(error) => Err(io::Error::from(error.kind())),
        };
        let _ = sender.send((read, write));
    });
    Ok(receiver)
}

fn retry<T>(until: Instant, mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match op() {
            Err(error) if error.raw_os_error() == Some(EAGAIN) && Instant::now() < until => {
                thread::sleep(Duration::from_millis(20));
            }
            other => return other,
        }
    }
}

/// Whether the echo got a full chunk in and wrote a full chunk back.
fn echoed(echo: &Receiver<(io::Result<usize>, io::Result<usize>)>) -> bool {
    matches!(
        echo.recv_timeout(LONG),
        Ok((Ok(read), Ok(written))) if read == CHUNK && written == CHUNK
    )
}

/// Whether a read is still waiting after [`STILL`].
fn still<T>(read: &Receiver<T>) -> bool {
    matches!(
        read.recv_timeout(STILL),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
}

/// Whether a queued AIO read is still waiting after [`STILL`]. An answer that came first is printed,
/// with what it was: the case that asked for a wait then has its answer.
fn still_aio(read: &Receiver<io::Result<Vec<u8>>>) -> bool {
    match read.recv_timeout(STILL) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => true,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => false,
        Ok(answer) => {
            eprintln!("holder: a queued AIO read answered before its event: {answer:?}");
            false
        }
    }
}

/// The errno of a read that answered, if it answered with one.
fn errno<T>(answer: Option<io::Result<T>>) -> Option<i32> {
    answer.and_then(|result| result.err().and_then(|error| error.raw_os_error()))
}
