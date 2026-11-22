//! `lemnos-restart-probe`: asks the restart helper's socket, for the systemd guest check
//! (`testing/qemu/run-systemd-restart.sh`).
//!
//! Usage:
//!   `lemnos-restart-probe check [SOCKET]`: `ok` and exit 0, or `refused: REASON` and exit 1.
//!   `lemnos-restart-probe reboot [SOCKET]`: prints `accepted` and then `stopping` (exit 0), or
//!     `failed: REASON` (exit 1). Nothing is printed after `stopping`.
//!   `lemnos-restart-probe raw LINE [SOCKET]`: sends LINE and a newline as they are, and prints the
//!     one answer line the helper gives (a garbage request must be refused).
//!
//! SOCKET is the helper's socket; the default is the packaged one.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use lemnos_restart::client::{Client, DEFAULT_SOCKET, Progress};

/// How long the probe waits for the helper to say the system is stopping after it was accepted.
const STOPPING_WAIT: Duration = Duration::from_secs(60);

fn say(line: &str) {
    let _ = writeln!(std::io::stdout(), "restart-probe: {line}");
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(verb) = args.next() else {
        return usage();
    };
    match verb.as_str() {
        "check" => {
            let client = client(args.next());
            match client.check() {
                Ok(()) => {
                    say("check: ok");
                    ExitCode::SUCCESS
                }
                Err(why) => {
                    say(&format!("check: refused: {why}"));
                    ExitCode::FAILURE
                }
            }
        }
        "reboot" => reboot(&client(args.next())),
        "pid1" => {
            let variant = args.next().unwrap_or_default();
            pid1(&variant, args.next())
        }
        "hold" => hold(args.next()),
        "gap" => {
            // gap MS COUNT [SOCKET]: COUNT fresh connections, BEGIN then the call MS ms later, each answer waited 2 s.
            let (Some(ms), Some(count)) = (args.next(), args.next()) else {
                return usage();
            };
            let (Ok(ms), Ok(count)) = (ms.parse::<u64>(), count.parse::<u32>()) else {
                return usage();
            };
            gap(ms, count, args.next())
        }
        "raw" => {
            let Some(line) = args.next() else {
                return usage();
            };
            raw(&line, args.next())
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    say("usage: check [SOCKET] | reboot [SOCKET] | raw LINE [SOCKET]");
    ExitCode::from(2)
}

fn client(socket: Option<String>) -> Client {
    Client {
        socket: PathBuf::from(socket.unwrap_or_else(|| DEFAULT_SOCKET.to_string())),
    }
}

fn reboot(client: &Client) -> ExitCode {
    let mut pending = match client.reboot() {
        Ok(p) => p,
        Err(why) => {
            say(&format!("reboot: refused: {why}"));
            return ExitCode::FAILURE;
        }
    };
    say("accepted");
    let start = Instant::now();
    loop {
        match pending.poll() {
            Progress::Stopping => {
                say("stopping");
                return ExitCode::SUCCESS;
            }
            Progress::Failed(why) => {
                say(&format!("failed: {why}"));
                return ExitCode::FAILURE;
            }
            Progress::Waiting => {
                if start.elapsed() >= STOPPING_WAIT {
                    say("failed: no answer after accepted within the probe's bound");
                    return ExitCode::FAILURE;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn raw(line: &str, socket: Option<String>) -> ExitCode {
    let socket = socket.unwrap_or_else(|| DEFAULT_SOCKET.to_string());
    let stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(e) => {
            say(&format!("raw: connect {socket}: {e}"));
            return ExitCode::FAILURE;
        }
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .is_err()
    {
        say("raw: no read timeout");
        return ExitCode::FAILURE;
    }
    let mut writer = &stream;
    // A helper that refuses before it reads the request closes the connection with the refusal already
    // sent: the write may fail, and the answer is still there to read.
    if let Err(e) = writer.write_all(format!("{line}\n").as_bytes()) {
        say(&format!("raw: send (the answer is read anyway): {e}"));
    }
    let mut answer = String::new();
    match BufReader::new(&stream).read_line(&mut answer) {
        Ok(0) => {
            say("raw: the helper closed the connection without an answer");
            ExitCode::FAILURE
        }
        Ok(_) => {
            say(&format!("raw: answer: {}", answer.trim_end()));
            ExitCode::SUCCESS
        }
        Err(e) => {
            say(&format!("raw: read: {e}"));
            ExitCode::FAILURE
        }
    }
}

/// The helper's LoadUnit("reboot.target") call, as the helper encodes it: a little-endian method call, serial 1,
/// with PATH, INTERFACE, MEMBER, DESTINATION and SIGNATURE header fields and one string argument.
fn load_unit_call(serial: u32) -> Vec<u8> {
    fn pad(out: &mut Vec<u8>, to: usize) {
        while !out.len().is_multiple_of(to) {
            out.push(0);
        }
    }
    fn put_str(out: &mut Vec<u8>, s: &str) {
        pad(out, 4);
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    }
    fn put_sig(out: &mut Vec<u8>, s: &str) {
        out.push(s.len() as u8);
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    }
    let mut body = Vec::new();
    put_str(&mut body, "reboot.target");
    let mut fields = Vec::new();
    for (code, sig, value) in [
        (1u8, "o", "/org/freedesktop/systemd1"),
        (2, "s", "org.freedesktop.systemd1.Manager"),
        (3, "s", "LoadUnit"),
        (6, "s", "org.freedesktop.systemd1"),
        (8, "g", "s"),
    ] {
        pad(&mut fields, 8);
        fields.push(code);
        put_sig(&mut fields, sig);
        if sig == "g" {
            put_sig(&mut fields, value);
        } else {
            put_str(&mut fields, value);
        }
    }
    let mut out = vec![b'l', 1, 0, 1];
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    out.extend_from_slice(&fields);
    pad(&mut out, 8);
    out.extend_from_slice(&body);
    out
}

/// Variant A: AUTH, NEGOTIATE, BEGIN and the call in one write. Variant B: BEGIN alone, the call 50 ms later.
/// The socket stays open for 30 s; the first answer's time (or none) is printed.
fn pid1(variant: &str, socket: Option<String>) -> ExitCode {
    let socket = socket.unwrap_or_else(|| "/run/systemd/private".to_string());
    let stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(e) => {
            say(&format!("pid1 {variant}: connect {socket}: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut writer = &stream;
    let mut reader = BufReader::new(&stream);
    let fail = |why: String| {
        say(&format!("pid1 {variant}: {why}"));
        ExitCode::FAILURE
    };
    if let Err(e) = writer.write_all(b"\0AUTH EXTERNAL 30\r\n") {
        return fail(format!("auth write: {e}"));
    }
    let mut line = String::new();
    if let Err(e) = reader.read_line(&mut line) {
        return fail(format!("auth read: {e}"));
    }
    say(&format!(
        "pid1 {variant}: auth answer {:?}",
        line.trim_end()
    ));
    if let Err(e) = writer.write_all(b"NEGOTIATE_UNIX_FD\r\n") {
        return fail(format!("negotiate write: {e}"));
    }
    line.clear();
    if let Err(e) = reader.read_line(&mut line) {
        return fail(format!("negotiate read: {e}"));
    }
    say(&format!(
        "pid1 {variant}: negotiate answer {:?}",
        line.trim_end()
    ));
    let call = load_unit_call(1);
    let start = Instant::now();
    match variant {
        "A" => {
            let mut one = b"BEGIN\r\n".to_vec();
            one.extend_from_slice(&call);
            if let Err(e) = writer.write_all(&one) {
                return fail(format!("batched write: {e}"));
            }
        }
        "B" => {
            if let Err(e) = writer.write_all(b"BEGIN\r\n") {
                return fail(format!("begin write: {e}"));
            }
            std::thread::sleep(Duration::from_millis(50));
            if let Err(e) = writer.write_all(&call) {
                return fail(format!("call write: {e}"));
            }
        }
        _ => return usage(),
    }
    let sent = start.elapsed();
    say(&format!(
        "pid1 {variant}: call sent at {} ms",
        sent.as_millis()
    ));
    // Hold the socket open for 30 s and report the first METHOD_RETURN or ERROR, if one comes.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut first: Option<(u8, u128)> = None;
    let mut reads = 0u32;
    while start.elapsed() < Duration::from_secs(30) {
        let mut fixed = [0u8; 16];
        match reader.read_exact(&mut fixed) {
            Ok(()) => {
                reads += 1;
                let body = u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]) as usize;
                let fields =
                    u32::from_le_bytes([fixed[12], fixed[13], fixed[14], fixed[15]]) as usize;
                // The rest of the message: the header fields, padded to eight, then the body.
                let after_fixed = (16 + fields).div_ceil(8) * 8 - 16 + body;
                let mut skip = vec![0u8; after_fixed];
                if reader.read_exact(&mut skip).is_err() {
                    break;
                }
                if (fixed[1] == 2 || fixed[1] == 3) && first.is_none() {
                    first = Some((fixed[1], start.elapsed().as_millis()));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    match first {
        Some((kind, ms)) => say(&format!(
            "pid1 {variant}: first answer type {kind} ({}) at {ms} ms; {reads} messages in 30 s",
            if kind == 2 { "METHOD_RETURN" } else { "ERROR" }
        )),
        None => say(&format!(
            "pid1 {variant}: no METHOD_RETURN or ERROR in 30 s ({reads} messages)"
        )),
    }
    ExitCode::SUCCESS
}

/// One exchange per connection: AUTH, NEGOTIATE, BEGIN, `ms` later the call. Returns the answer's latency in ms, or None
/// when no METHOD_RETURN or ERROR came within 2 s.
fn one_exchange(socket: &str, ms: u64) -> Result<Option<u128>, String> {
    let stream = UnixStream::connect(socket).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|e| format!("timeout: {e}"))?;
    let mut writer = &stream;
    writer
        .write_all(b"\0AUTH EXTERNAL 30\r\n")
        .map_err(|e| format!("auth: {e}"))?;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("auth read: {e}"))?;
    writer
        .write_all(b"NEGOTIATE_UNIX_FD\r\n")
        .map_err(|e| format!("negotiate: {e}"))?;
    line.clear();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("negotiate read: {e}"))?;
    writer
        .write_all(b"BEGIN\r\n")
        .map_err(|e| format!("begin: {e}"))?;
    std::thread::sleep(Duration::from_millis(ms));
    let start = Instant::now();
    writer
        .write_all(&load_unit_call(1))
        .map_err(|e| format!("call: {e}"))?;
    while start.elapsed() < Duration::from_secs(2) {
        let mut fixed = [0u8; 16];
        match reader.read_exact(&mut fixed) {
            Ok(()) => {
                let body = u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]) as usize;
                let fields =
                    u32::from_le_bytes([fixed[12], fixed[13], fixed[14], fixed[15]]) as usize;
                let after_fixed = (16 + fields).div_ceil(8) * 8 - 16 + body;
                let mut skip = vec![0u8; after_fixed];
                reader
                    .read_exact(&mut skip)
                    .map_err(|e| format!("read: {e}"))?;
                if fixed[1] == 2 || fixed[1] == 3 {
                    return Ok(Some(start.elapsed().as_millis()));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    Ok(None)
}

fn gap(ms: u64, count: u32, socket: Option<String>) -> ExitCode {
    let socket = socket.unwrap_or_else(|| "/run/systemd/private".to_string());
    let (mut ok, mut worst, mut errors) = (0u32, 0u128, 0u32);
    for _ in 0..count {
        match one_exchange(&socket, ms) {
            Ok(Some(t)) => {
                ok += 1;
                worst = worst.max(t);
            }
            Ok(None) => {}
            Err(why) => {
                errors += 1;
                say(&format!("gap {ms}: {why}"));
            }
        }
    }
    say(&format!(
        "gap {ms} ms: {ok} of {count} answered within 2 s (worst {worst} ms, {errors} errors)"
    ));
    ExitCode::SUCCESS
}

/// A method call with the given header fields and body, little-endian, as the helper encodes it.
fn encode_msg(serial: u32, path: &str, interface: &str, member: &str, body: &[u8]) -> Vec<u8> {
    fn pad(out: &mut Vec<u8>, to: usize) {
        while !out.len().is_multiple_of(to) {
            out.push(0);
        }
    }
    fn put_str(out: &mut Vec<u8>, s: &str) {
        pad(out, 4);
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    }
    fn put_sig(out: &mut Vec<u8>, s: &str) {
        out.push(s.len() as u8);
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    }
    let mut fields = Vec::new();
    for (code, sig, value) in [
        (1u8, "o", path),
        (2, "s", interface),
        (3, "s", member),
        (6, "s", "org.freedesktop.systemd1"),
    ] {
        pad(&mut fields, 8);
        fields.push(code);
        put_sig(&mut fields, sig);
        put_str(&mut fields, value);
    }
    let mut out = vec![b'l', 1, 0, 1];
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    out.extend_from_slice(&fields);
    pad(&mut out, 8);
    out.extend_from_slice(body);
    out
}

/// Reads one message: its type, its serial, and its REPLY_SERIAL if it has one.
fn read_one<R: Read>(reader: &mut R) -> Result<(u8, u32, Option<u32>), String> {
    let mut fixed = [0u8; 16];
    reader.read_exact(&mut fixed).map_err(|e| e.to_string())?;
    let body = u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]) as usize;
    let fields = u32::from_le_bytes([fixed[12], fixed[13], fixed[14], fixed[15]]) as usize;
    let mut rest = vec![0u8; (16 + fields).div_ceil(8) * 8 - 16 + body];
    reader.read_exact(&mut rest).map_err(|e| e.to_string())?;
    // The fields: each struct starts on an 8-byte boundary; REPLY_SERIAL (5) is a uint32.
    let bytes = &rest[..fields];
    let (mut at, mut reply) = (0usize, None);
    while at < fields {
        at = at.div_ceil(8) * 8;
        if at >= fields {
            break;
        }
        let code = bytes[at];
        let sig_len = bytes[at + 1] as usize;
        let sig = &bytes[at + 2..at + 2 + sig_len];
        at += 2 + sig_len + 1;
        match sig {
            b"u" => {
                at = at.div_ceil(4) * 4;
                let v =
                    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                if code == 5 {
                    reply = Some(v);
                }
                at += 4;
            }
            b"s" | b"o" => {
                at = at.div_ceil(4) * 4;
                let n = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                    as usize;
                at += 4 + n + 1;
            }
            b"g" => {
                let n = bytes[at] as usize;
                at += 1 + n + 1;
            }
            _ => {
                return Err(format!(
                    "header field signature {:?}",
                    String::from_utf8_lossy(sig)
                ));
            }
        }
    }
    Ok((
        fixed[1],
        u32::from_le_bytes([fixed[8], fixed[9], fixed[10], fixed[11]]),
        reply,
    ))
}

/// HOLD: AUTH, NEGOTIATE, then BEGIN and the LoadUnit call written back to back (gap 0). One second later a Ping
/// (serial 2) is written. Prints every message the manager sends for 3 s, with its time, type and REPLY_SERIAL.
fn hold(socket: Option<String>) -> ExitCode {
    let socket = socket.unwrap_or_else(|| "/run/systemd/private".to_string());
    let stream = match UnixStream::connect(&socket) {
        Ok(s) => s,
        Err(e) => {
            say(&format!("hold: connect: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = &stream;
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if writer.write_all(b"\0AUTH EXTERNAL 30\r\n").is_err() || reader.read_line(&mut line).is_err()
    {
        say("hold: auth failed");
        return ExitCode::FAILURE;
    }
    line.clear();
    if writer.write_all(b"NEGOTIATE_UNIX_FD\r\n").is_err() || reader.read_line(&mut line).is_err() {
        say("hold: negotiate failed");
        return ExitCode::FAILURE;
    }
    let start = Instant::now();
    if writer.write_all(b"BEGIN\r\n").is_err() {
        say("hold: begin failed");
        return ExitCode::FAILURE;
    }
    let call = load_unit_call(1);
    if writer.write_all(&call).is_err() {
        say("hold: call failed");
        return ExitCode::FAILURE;
    }
    say(&format!(
        "hold: BEGIN and call (serial 1) written at {} ms",
        start.elapsed().as_millis()
    ));
    std::thread::sleep(Duration::from_secs(1));
    let ping = encode_msg(2, "/", "org.freedesktop.DBus.Peer", "Ping", &[]);
    if writer.write_all(&ping).is_err() {
        say("hold: ping write failed");
        return ExitCode::FAILURE;
    }
    say(&format!(
        "hold: Ping (serial 2) written at {} ms",
        start.elapsed().as_millis()
    ));
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    while start.elapsed() < Duration::from_secs(4) {
        match read_one(&mut reader) {
            Ok((kind, serial, reply)) => say(&format!(
                "hold: at {} ms message type {kind} serial {serial} reply_serial {reply:?}",
                start.elapsed().as_millis()
            )),
            Err(e)
                if e.contains("WouldBlock")
                    || e.contains("TimedOut")
                    || e.contains("temporarily") => {}
            Err(e) => {
                say(&format!("hold: read ended: {e}"));
                break;
            }
        }
    }
    ExitCode::SUCCESS
}
