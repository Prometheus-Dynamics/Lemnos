//! The few D-Bus method calls the helper makes to the service manager over its private socket
//! (`/run/systemd/private`): a direct connection, so there is no bus daemon and no `Hello`. Authentication is `EXTERNAL` with this process's uid, which the manager checks
//! against the socket's peer credentials. Only little-endian messages are read; a manager that
//! answers in the other byte order is refused by name.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long one answer from the manager may take. It also bounds the handshake's ping loop: the sd-bus citation for why that
/// loop exists is at `PING_FIRST`.
const ANSWER_WAIT: Duration = Duration::from_secs(5);
/// The first wait for the answer to a Ping sent after BEGIN, and the longest a wait grows to (the failure rule's cap).
/// Why the handshake sends Pings until one is answered (upstream systemd v259.10, commit cc6062f2): the manager copies the
/// bytes after BEGIN into its read buffer (src/libsystemd/sd-bus/bus-socket.c:446-447, then bus_start_running). Its event
/// loop then waits on the socket only: sd_bus_get_events asks for POLLIN by the reply queue, never by that buffer
/// (sd-bus.c:2588), and each io wakeup runs one sd_bus_process (sd-bus.c:3651). The auth step returns without dispatching
/// (sd-bus.c:3281, 3286), so a call that shared a read with BEGIN waits until the next bytes arrive. Each Ping is such bytes.
/// Fedora's systemd 259.10 build may carry patches; they were not checked. No deterministic client-side alternative was
/// found in the source (see `Manager::ping`).
const PING_FIRST: Duration = Duration::from_millis(100);
const PING_MAX: Duration = Duration::from_secs(8);
const PEER: &str = "org.freedesktop.DBus.Peer";
/// The largest message read from the manager; anything larger is refused.
const MAX_MESSAGE: usize = 1 << 20;

const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
/// The manager's bus name, named as every call's destination (see [`Manager::connect`]).
const DESTINATION: &str = "org.freedesktop.systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const METHOD_CALL: u8 = 1;
const METHOD_RETURN: u8 = 2;
const ERROR: u8 = 3;

/// A connection to the service manager.
pub struct Manager {
    stream: BufReader<UnixStream>,
    serial: u32,
}

/// Why a call did not give its answer: the manager's own error (its name and text), or the
/// connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The manager answered with an error.
    Refused { name: String, text: String },
    /// The connection failed, or the answer was not one this client reads.
    Link(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Refused { name, text } => write!(f, "{name}: {text}"),
            CallError::Link(why) => f.write_str(why),
        }
    }
}

fn link(e: io::Error) -> CallError {
    CallError::Link(e.to_string())
}

impl Manager {
    /// Connects to the manager's private socket at `path` and authenticates as `uid`.
    pub fn connect(path: &Path, uid: u32) -> Result<Self, CallError> {
        let stream = UnixStream::connect(path)
            .map_err(|e| CallError::Link(format!("{}: {e}", path.display())))?;
        Self::over(stream, uid, path)
    }

    /// Authenticates as `uid` over a connected `stream` to the manager at `path` (named in errors).
    fn over(stream: UnixStream, uid: u32, path: &Path) -> Result<Self, CallError> {
        stream.set_read_timeout(Some(ANSWER_WAIT)).map_err(link)?;
        stream.set_write_timeout(Some(ANSWER_WAIT)).map_err(link)?;
        let mut stream = BufReader::new(stream);
        let hex: String = uid
            .to_string()
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        stream
            .get_mut()
            .write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())
            .map_err(link)?;
        let mut line = String::new();
        stream.read_line(&mut line).map_err(link)?;
        if !line.starts_with("OK ") {
            return Err(CallError::Link(format!(
                "{}: the manager did not accept uid {uid}: {:?}",
                path.display(),
                line.trim_end()
            )));
        }
        // Found against systemd 259's manager: a method call is answered reliably only on a
        // connection that negotiated descriptor passing and when the call names the manager as its
        // destination; without either, answers came sometimes or never. Nothing here passes a
        // descriptor.
        stream
            .get_mut()
            .write_all(b"NEGOTIATE_UNIX_FD\r\n")
            .map_err(link)?;
        line.clear();
        stream.read_line(&mut line).map_err(link)?;
        if line.trim_end() != "AGREE_UNIX_FD" {
            return Err(CallError::Link(format!(
                "{}: the manager did not agree to the connection's terms: {:?}",
                path.display(),
                line.trim_end()
            )));
        }
        stream.get_mut().write_all(b"BEGIN\r\n").map_err(link)?;
        let mut manager = Self { stream, serial: 0 };
        manager.ping()?;
        Ok(manager)
    }

    /// `LoadUnit(name)`: the unit's object path.
    pub fn load_unit(&mut self, name: &str) -> Result<String, CallError> {
        let body = self.call(MANAGER_PATH, MANAGER, "LoadUnit", "s", &[Arg::Str(name)])?;
        body.object()
    }

    /// `StartUnit(name, mode)`: the job's object path.
    pub fn start_unit(&mut self, name: &str, mode: &str) -> Result<String, CallError> {
        let body = self.call(
            MANAGER_PATH,
            MANAGER,
            "StartUnit",
            "ss",
            &[Arg::Str(name), Arg::Str(mode)],
        )?;
        body.object()
    }

    /// A string property of the object at `path` (`Properties.Get(interface, name)`).
    pub fn string_property(
        &mut self,
        path: &str,
        interface: &str,
        name: &str,
    ) -> Result<String, CallError> {
        let body = self.call(
            path,
            PROPERTIES,
            "Get",
            "ss",
            &[Arg::Str(interface), Arg::Str(name)],
        )?;
        body.variant_string()
    }

    /// The manager's `SystemState` (`running`, `stopping`, ...).
    pub fn system_state(&mut self) -> Result<String, CallError> {
        self.string_property(MANAGER_PATH, MANAGER, "SystemState")
    }

    /// Pings the manager until one of its own Pings is answered. Each Ping is a new write, so the manager reads it
    /// (the mechanism is at `PING_FIRST`), and a Ping not answered within the current wait is sent again with a new serial
    /// and a doubled wait (capped). Replies to earlier Pings or signals are skipped. Bounded by ANSWER_WAIT.
    fn ping(&mut self) -> Result<(), CallError> {
        let deadline = Instant::now() + ANSWER_WAIT;
        let mut gap = PING_FIRST;
        let mut sent: Vec<u32> = Vec::new();
        loop {
            if Instant::now() >= deadline {
                return Err(CallError::Link(format!(
                    "the manager did not answer a ping within {} ms",
                    ANSWER_WAIT.as_millis()
                )));
            }
            self.serial += 1;
            let serial = self.serial;
            let message = encode_message(serial, "/", PEER, "Ping", Some(DESTINATION), "", &[]);
            self.stream.get_mut().write_all(&message).map_err(link)?;
            sent.push(serial);
            let window_end = (Instant::now() + gap).min(deadline);
            loop {
                let left = window_end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                self.stream
                    .get_ref()
                    .set_read_timeout(Some(left))
                    .map_err(link)?;
                // Waiting for a first byte consumes nothing, so a timeout here cannot split a message.
                match self.stream.fill_buf() {
                    Ok([]) => {
                        return Err(CallError::Link("the manager closed the connection".into()));
                    }
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(e) => return Err(link(e)),
                }
                self.stream
                    .get_ref()
                    .set_read_timeout(Some(ANSWER_WAIT))
                    .map_err(link)?;
                let reply = read_message(&mut self.stream)?;
                if reply.kind == METHOD_RETURN
                    && reply.reply_serial.is_some_and(|r| sent.contains(&r))
                {
                    return Ok(());
                }
            }
            gap = (gap * 2).min(PING_MAX);
        }
    }

    fn call(
        &mut self,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        args: &[Arg<'_>],
    ) -> Result<Body, CallError> {
        self.serial += 1;
        let serial = self.serial;
        let message = encode_call(serial, path, interface, member, signature, args);
        self.stream.get_mut().write_all(&message).map_err(link)?;
        // Skip anything that is not the answer to this call: the manager sends its signals to every
        // direct connection, so the answer is waited for by its own deadline, not by the socket's.
        let deadline = std::time::Instant::now() + ANSWER_WAIT;
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(CallError::Link(format!(
                    "the manager did not answer {member} within {} ms",
                    ANSWER_WAIT.as_millis()
                )));
            }
            let reply = read_message(&mut self.stream)?;
            if reply.reply_serial != Some(serial) {
                continue;
            }
            return match reply.kind {
                METHOD_RETURN => Ok(Body {
                    signature: reply.signature,
                    bytes: reply.body,
                }),
                ERROR => {
                    let text = Body {
                        signature: reply.signature,
                        bytes: reply.body,
                    }
                    .string()
                    .unwrap_or_default();
                    Err(CallError::Refused {
                        name: reply.error_name.unwrap_or_else(|| "(unnamed error)".into()),
                        text,
                    })
                }
                other => Err(CallError::Link(format!(
                    "the manager answered {member} with message type {other}"
                ))),
            };
        }
    }
}

enum Arg<'a> {
    Str(&'a str),
}

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

/// A method call's bytes: the fixed header, the header fields (path, interface, member,
/// destination and, with arguments, the signature), padding to eight, then the body.
fn encode_call(
    serial: u32,
    path: &str,
    interface: &str,
    member: &str,
    signature: &str,
    args: &[Arg<'_>],
) -> Vec<u8> {
    encode_message(
        serial,
        path,
        interface,
        member,
        Some(DESTINATION),
        signature,
        args,
    )
}

/// A method call's bytes, with the destination when there is one.
fn encode_message(
    serial: u32,
    path: &str,
    interface: &str,
    member: &str,
    destination: Option<&str>,
    signature: &str,
    args: &[Arg<'_>],
) -> Vec<u8> {
    let mut body = Vec::new();
    for arg in args {
        match arg {
            Arg::Str(s) => put_str(&mut body, s),
        }
    }
    let mut fields = Vec::new();
    let mut field = |code: u8, sig: &str, value: &str| {
        pad(&mut fields, 8);
        fields.push(code);
        put_sig(&mut fields, sig);
        if sig == "g" {
            put_sig(&mut fields, value);
        } else {
            put_str(&mut fields, value);
        }
    };
    field(1, "o", path);
    field(2, "s", interface);
    field(3, "s", member);
    if let Some(destination) = destination {
        field(6, "s", destination);
    }
    if !signature.is_empty() {
        field(8, "g", signature);
    }
    let mut out = vec![b'l', METHOD_CALL, 0, 1];
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    out.extend_from_slice(&fields);
    pad(&mut out, 8);
    out.extend_from_slice(&body);
    out
}

/// A message read from the manager: what this client looks at.
struct Message {
    kind: u8,
    /// The sender's serial: checked by the replay tests against the recorded bytes.
    #[cfg_attr(not(test), allow(dead_code))]
    serial: u32,
    member: Option<String>,
    destination: Option<String>,
    reply_serial: Option<u32>,
    error_name: Option<String>,
    signature: String,
    body: Vec<u8>,
}

fn malformed(what: &str) -> CallError {
    CallError::Link(format!("the manager's answer is malformed: {what}"))
}

/// A reader over a message's bytes that keeps D-Bus alignment (offsets are from the message start).
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn align(&mut self, to: usize) {
        self.at = self.at.div_ceil(to) * to;
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], CallError> {
        let end = self.at.checked_add(n).ok_or_else(|| malformed("length"))?;
        let s = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| malformed("short"))?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, CallError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, CallError> {
        self.align(4);
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn string(&mut self) -> Result<String, CallError> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        if self.u8()? != 0 {
            return Err(malformed("string without its NUL"));
        }
        String::from_utf8(s.to_vec()).map_err(|_| malformed("string is not UTF-8"))
    }
    fn signature(&mut self) -> Result<String, CallError> {
        let n = self.u8()? as usize;
        let s = self.take(n)?;
        if self.u8()? != 0 {
            return Err(malformed("signature without its NUL"));
        }
        String::from_utf8(s.to_vec()).map_err(|_| malformed("signature is not UTF-8"))
    }
    /// Skips one value of the single complete type `sig` that the header fields use.
    fn value(&mut self, sig: &str) -> Result<Option<String>, CallError> {
        Ok(match sig {
            "s" | "o" => Some(self.string()?),
            "g" => Some(self.signature()?),
            "u" => Some(self.u32()?.to_string()),
            "y" => Some(self.u8()?.to_string()),
            other => return Err(malformed(&format!("header field of type {other:?}"))),
        })
    }
}

fn read_message<R: Read>(stream: &mut R) -> Result<Message, CallError> {
    let mut fixed = [0u8; 16];
    stream.read_exact(&mut fixed).map_err(link)?;
    if fixed[0] != b'l' {
        return Err(CallError::Link(format!(
            "the manager answered in byte order {:?}; this client reads only little-endian",
            fixed[0] as char
        )));
    }
    let body_len = u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]) as usize;
    let fields_len = u32::from_le_bytes([fixed[12], fixed[13], fixed[14], fixed[15]]) as usize;
    let header_end = (16 + fields_len).div_ceil(8) * 8;
    let total = header_end
        .checked_add(body_len)
        .filter(|t| *t <= MAX_MESSAGE)
        .ok_or_else(|| malformed("larger than this client reads"))?;
    let mut bytes = vec![0u8; total];
    bytes[..16].copy_from_slice(&fixed);
    stream.read_exact(&mut bytes[16..]).map_err(link)?;
    let mut c = Cursor {
        bytes: &bytes[..16 + fields_len],
        at: 16,
    };
    let mut message = Message {
        kind: fixed[1],
        serial: u32::from_le_bytes([fixed[8], fixed[9], fixed[10], fixed[11]]),
        member: None,
        destination: None,
        reply_serial: None,
        error_name: None,
        signature: String::new(),
        body: bytes[header_end..].to_vec(),
    };
    while c.at < 16 + fields_len {
        c.align(8);
        if c.at >= 16 + fields_len {
            break;
        }
        let code = c.u8()?;
        let sig = c.signature()?;
        let value = c.value(&sig)?;
        match code {
            3 => message.member = value,
            4 => message.error_name = value,
            6 => message.destination = value,
            5 => {
                message.reply_serial = value.and_then(|v| v.parse().ok());
            }
            8 => message.signature = value.unwrap_or_default(),
            _ => {}
        }
    }
    Ok(message)
}

/// A reply's body, read by the one shape each call expects.
struct Body {
    signature: String,
    bytes: Vec<u8>,
}

impl Body {
    fn first(&self, want: &str) -> Result<Cursor<'_>, CallError> {
        if !self.signature.starts_with(want) {
            return Err(malformed(&format!(
                "body of signature {:?}, expected {want:?}",
                self.signature
            )));
        }
        Ok(Cursor {
            bytes: &self.bytes,
            at: 0,
        })
    }
    fn string(&self) -> Result<String, CallError> {
        self.first("s")?.string()
    }
    fn object(&self) -> Result<String, CallError> {
        self.first("o")?.string()
    }
    fn variant_string(&self) -> Result<String, CallError> {
        let mut c = self.first("v")?;
        let sig = c.signature()?;
        if sig != "s" {
            return Err(malformed(&format!(
                "property of type {sig:?}, expected a string"
            )));
        }
        c.string()
    }
}

#[cfg(test)]
#[path = "dbus_tests.rs"]
mod tests;
