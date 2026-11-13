#![allow(clippy::print_stderr)] // the harness reports its steps on stderr, by design
//! The holder checks: what a process that holds the gadget's bulk pair (R1b, as Orion does) sees
//! when the host or the daemon changes the function underneath it. The holder keeps copies of the
//! two endpoint descriptors and reads and writes them itself.
//!
//! Each case has the same shape. Before the event, a read is waiting on the configured host (a),
//! and a queued AIO read (b) is under way (the backend's own form). The event comes next. Then
//! the errno of each read in flight is checked, and a new read (d) and a new write (c) made after
//! the event are checked too, before the same descriptors carry a chunk again.
//!
//! The events come from [`Hub`] (the root side: port power, suspend, reset, configuration, halt,
//! mount) and from the daemon's own calls (`disable`, the UDC attribute), which this process makes
//! as the daemon's user.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_hal::UsbFunction;
use lemnos_linux::FunctionFsGadget;
use lemnos_linux_sys::aio::Queue;

use crate::hub::Hub;
use crate::{CHUNK, GADGET_DIR, UDC};

/// `ESHUTDOWN`: a request in flight when the endpoint was disabled.
const ESHUTDOWN: i32 = 108;
/// `EAGAIN`: a request on a disabled endpoint, on a descriptor that is non-blocking.
const EAGAIN: i32 = 11;
/// `EBUSY`: a mount or unmount while the instance is still in use.
const EBUSY: i32 = 16;
/// How long a read is expected to still be waiting.
const STILL: Duration = Duration::from_millis(500);
/// How long a read that should answer at once may take.
const SHORT: Duration = Duration::from_secs(3);
/// The longest any step waits for the host.
const LONG: Duration = Duration::from_secs(20);

/// Runs every case. `check` gets each assertion's result and text, and returns it.
pub fn run(
    gadget: &mut FunctionFsGadget,
    hub: &mut Hub,
    check: &mut dyn FnMut(bool, &str) -> bool,
) {
    if let Err(error) = cases(gadget, hub, &mut *check) {
        check(false, &format!("the holder checks ran to the end: {error}"));
    }
}

fn cases(
    gadget: &mut FunctionFsGadget,
    hub: &mut Hub,
    check: &mut dyn FnMut(bool, &str) -> bool,
) -> io::Result<()> {
    let [out, input] = lend(gadget)?;
    check(true, "the holder is lent the bulk pair");

    // 1. The host disconnects (port power off) and connects again (power on). A sync read, then
    // a queued AIO read on its own: a second reader on the same endpoint would share its request.
    let waiting = read_once(&out)?;
    check(still(&waiting), "1 (a) a read on a configured host waits");
    hub.ask("off")?;
    let inflight = errno(waiting.recv_timeout(LONG).ok());
    check(
        inflight == Some(ESHUTDOWN),
        &format!("1 (a) the read in flight at a disconnect fails ESHUTDOWN (got {inflight:?})"),
    );
    let fresh = errno(read_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!("1 (d) a new read while the host is away fails EAGAIN at once (got {fresh:?})"),
    );
    let written = errno(write_once(&input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("1 (c) a write while the host is away fails EAGAIN (got {written:?})"),
    );
    hub.ask("on")?;
    let echo = spawn_echo(&out, &input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "1 after the host connects again, the same descriptors carry the bytes",
    );
    let queued = aio_once(&out)?;
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
    let fresh_aio = errno(aio_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("1 (b) a new AIO read while the host is away fails EAGAIN (got {fresh_aio:?})"),
    );
    hub.ask("on")?;
    let echo = spawn_echo(&out, &input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "1 (b) after the host connects again the same descriptors carry the bytes",
    );

    // 2a. The host suspends and resumes; the read waiting through it takes the bytes, and a new read
    // after the resume does too.
    let echo = spawn_echo(&out, &input)?;
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
    let echo = spawn_echo(&out, &input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "2a (d) a new read after the resume takes the host's bytes and the write answers",
    );

    // 2b. The host resets the bus: the read in flight fails, and a read issued just after the reset
    // either fails EAGAIN (the host has not configured yet) or waits, and then takes the host's
    // chunk and writes it back itself, so the transfer completes.
    let echo = spawn_echo(&out, &input)?;
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
    let after_reset = read_once(&out)?;
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
        let mut writer = File::from(input.try_clone()?);
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
    let echo = spawn_echo(&out, &input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "2b after the reset and the host's configuration, the same descriptors carry the bytes",
    );

    // 3. The host sets configuration 0 (the function is disabled), then 1 (enabled again).
    let echo = spawn_echo(&out, &input)?;
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
    let fresh = errno(read_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!("3 (d) a new read while unconfigured fails EAGAIN (got {fresh:?})"),
    );
    let fresh_aio = errno(aio_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("3 (b) a new AIO read while unconfigured fails EAGAIN (got {fresh_aio:?})"),
    );
    let written = errno(write_once(&input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("3 (c) a write while unconfigured fails EAGAIN (got {written:?})"),
    );
    let echo = spawn_echo(&out, &input)?;
    hub.ask("cfg 1")?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "3 after configuration 1 the same descriptors carry the bytes",
    );

    // 4. The gadget is unbound from its controller and bound again; the daemon keeps ep0 open. A
    // sync read in flight first, then a queued AIO read on its own.
    let echo = spawn_echo(&out, &input)?;
    check(still(&echo), "4 (a) the read is waiting before the unbind");
    fs::write(format!("{GADGET_DIR}/UDC"), "\n")?;
    let (read, _) = echo.recv_timeout(LONG).map_err(io::Error::other)?;
    let unbound = read.err().and_then(|e| e.raw_os_error());
    check(
        unbound == Some(ESHUTDOWN),
        &format!("4 (a) the read in flight at the unbind fails ESHUTDOWN (got {unbound:?})"),
    );
    let fresh = errno(read_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!("4 (d) a new read while unbound fails EAGAIN, not ENODEV (got {fresh:?})"),
    );
    let written = errno(write_once(&input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("4 (c) a write while unbound fails EAGAIN (got {written:?})"),
    );
    fs::write(format!("{GADGET_DIR}/UDC"), format!("{UDC}\n"))?;
    let echo = spawn_echo(&out, &input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "4 after the rebind and the host's configuration, the same descriptors carry the bytes",
    );
    let queued = aio_once(&out)?;
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
    let fresh_aio = errno(aio_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("4 (b) a new AIO read while unbound fails EAGAIN (got {fresh_aio:?})"),
    );
    fs::write(format!("{GADGET_DIR}/UDC"), format!("{UDC}\n"))?;
    let echo = spawn_echo(&out, &input)?;
    hub.ask("bulk")?;
    check(
        echoed(&echo),
        "4 (b) after the rebind the same descriptors carry the bytes",
    );

    // Halt: the host halts the bulk OUT endpoint. A read waiting on it keeps waiting (no code),
    // and it takes the bytes once the host clears the halt.
    let echo = spawn_echo(&out, &input)?;
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

    // 5. The daemon tears the gadget down: ep0 and its files closed, the mount removed. The queued
    // AIO read is the one in flight at the teardown (a sync read and an AIO read would share the
    // endpoint's request, and the teardown is done once per run).
    let queued = aio_once(&out)?;
    check(
        still_aio(&queued),
        "5 (b) the queued AIO read is waiting before the teardown",
    );
    gadget.disable().map_err(hal)?;
    let queued_err = errno(queued.recv_timeout(LONG).ok());
    check(
        queued_err == Some(ESHUTDOWN),
        &format!("5 (b) the queued AIO read at the teardown fails ESHUTDOWN (got {queued_err:?})"),
    );
    let umounted = hub.ask("umount");
    check(
        umounted.as_ref().err().and_then(|e| e.raw_os_error()) == Some(EBUSY),
        &format!("5 unmount with the holder's files open is EBUSY (got {umounted:?})"),
    );
    let fresh = errno(read_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh == Some(EAGAIN),
        &format!(
            "5 (d) a new read after the daemon's close fails EAGAIN, descriptors live (got {fresh:?})"
        ),
    );
    let fresh_aio = errno(aio_once(&out)?.recv_timeout(SHORT).ok());
    check(
        fresh_aio == Some(EAGAIN),
        &format!("5 (b) a new AIO read after the daemon's close fails EAGAIN (got {fresh_aio:?})"),
    );
    let written = errno(write_once(&input)?.recv_timeout(SHORT).ok());
    check(
        written == Some(EAGAIN),
        &format!("5 (c) a write after the daemon's close fails EAGAIN (got {written:?})"),
    );
    hub.ask("detach")?;
    let remount = hub.ask("mount");
    check(
        remount.as_ref().err().and_then(|e| e.raw_os_error()) == Some(EBUSY),
        &format!(
            "5 a new offer (mount) while the holder keeps the pair is EBUSY (got {remount:?})"
        ),
    );
    drop(out);
    drop(input);
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

/// A driver error as an `io::Error`, for the holder checks to report.
fn hal(error: lemnos_hal::ErrorKind) -> io::Error {
    io::Error::other(format!("{error:?}"))
}

/// The two bulk descriptors, as copies the holder keeps: OUT first, then IN.
fn lend(gadget: &FunctionFsGadget) -> io::Result<[OwnedFd; 2]> {
    let handles = gadget.endpoint_handles().map_err(hal)?;
    Ok([
        handles[0].fd().try_clone_to_owned()?,
        handles[1].fd().try_clone_to_owned()?,
    ])
}

/// One read of up to a chunk on a copy of `fd`, on its own thread, so the caller can wait on it
/// with a timeout. The answer comes once: the data, or the errno.
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
