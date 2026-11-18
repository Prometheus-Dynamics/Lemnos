//! How a trial resolves: the attempts a trial has and what each one did ([`Log`]), what resolved
//! last ([`Resolved`]), the board a trial is judged on ([`Dev`], [`regression`]) and the sources
//! of health the board file names ([`Source`]). Everything here is a pure decision or a small
//! file; the driver in [`crate`] makes the calls.
//!
//! A trial's attempts are the boots that may still start it. The loader takes one before each
//! trial boot, and the daemon gives its attempt back once it runs in the trial slot ([`Log`]
//! keeps what the driver last left). A boot that never reaches the daemon keeps its attempt
//! used, so three of them in a row fall back.

use std::fs;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lemnos_core::{BoardDevice, Health, Origin};

use crate::json::quoted;
use crate::store::{Layout, now_utc, replace};

/// What a trial is judged on. Built from the board file's `system-slots` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The board confirms a healthy trial itself.
    pub auto_confirm: bool,
    /// How long the daemon must have run in the trial before it may confirm.
    pub uptime: Duration,
    /// How long from the daemon's start until every condition must hold.
    pub deadline: Duration,
    /// At the deadline with conditions unmet, restart plainly, so the default comes back.
    pub reboot_at_deadline: bool,
    /// Boots allowed to start a trial before it falls back (1 to 3).
    pub attempts: u8,
    /// The board file's `health` list, as written.
    pub health: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            auto_confirm: true,
            uptime: Duration::from_secs(30),
            deadline: Duration::from_secs(600),
            reboot_at_deadline: true,
            attempts: lemnos_slot_store::records::MAX_ATTEMPTS,
            health: Vec::new(),
        }
    }
}

/// A condition on the health of the board, from the board file's `health` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `device:<id>`: that device's health is Ok.
    Device(String),
    /// `file:<path>`: the file exists and was modified since this boot began.
    File(PathBuf),
    /// `socket:<path>`: a Unix socket that accepts a connection.
    Socket(PathBuf),
    /// `tcp:<address>:<port>`: a TCP port that accepts a connection.
    Tcp(Tcp),
}

/// A `tcp:` source: an address and a port, never a name (nothing here looks a name up).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tcp {
    /// The entry after `tcp:`, as written: what a reason names.
    text: String,
    addr: TcpAddr,
    port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TcpAddr {
    V4(Ipv4Addr),
    /// With the zone of a link-local address: an interface's name, which is looked up at each
    /// look, since the interface may appear after the daemon starts.
    V6(Ipv6Addr, Option<String>),
}

/// Why an entry of the `health` list is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The sentence, naming the entry.
    pub sentence: String,
    /// The same without the entry, in words that follow the key `health`: what the board-file
    /// check says.
    pub reason: &'static str,
}

impl Refused {
    fn new(text: &str, reason: &'static str) -> Self {
        Self {
            sentence: format!("health entry `{text}` is refused: `health` {reason}"),
            reason,
        }
    }
}

/// The refusal of an entry that is none of the sources, or a device or path source gone wrong.
pub const NOT_A_SOURCE: &str =
    "entries must be device:<id>, file:<path>, socket:<path> or tcp:<address>:<port>";
/// The refusals of a `tcp:` entry.
pub const TCP_SHAPE: &str = "has a tcp entry that is not tcp:<address>:<port>";
pub const TCP_PORT: &str = "has a tcp entry whose port is not a number from 1 to 65535";
pub const TCP_BRACKETS: &str =
    "has a tcp entry whose IPv6 address is not in brackets: tcp:[<address>]:<port>";
pub const TCP_NOT_V6: &str = "has a tcp entry whose brackets do not hold an IPv6 address";
pub const TCP_HOST_NAME: &str = "has a tcp entry that names a host: only an IPv4 address or an IPv6 address in brackets is taken, no name is looked up";
pub const TCP_ZONE: &str =
    "has a tcp entry whose zone is not an interface name (1 to 15 characters after the %)";
pub const TCP_NEEDS_ZONE: &str = "has a tcp entry with a link-local IPv6 address and no zone: tcp:[<address>%<interface>]:<port>";
pub const TCP_ZONE_NOT_LINK_LOCAL: &str =
    "has a tcp entry with a zone on an address that is not link-local (fe80::/10)";

/// The longest interface name the kernel takes (`IFNAMSIZ` less its NUL).
const ZONE_MAX: usize = 15;

impl Tcp {
    /// Reads what follows `tcp:`. Only the shape is checked: whether the address is on the
    /// board, and whether a zone's interface exists, is found at each look.
    fn parse(rest: &str) -> Result<Self, &'static str> {
        let (addr, port) = if let Some(inner) = rest.strip_prefix('[') {
            let (inner, after) = inner.split_once(']').ok_or(TCP_NOT_V6)?;
            let port = after.strip_prefix(':').ok_or(TCP_SHAPE)?;
            let (ip, zone) = match inner.split_once('%') {
                Some((ip, zone)) => (ip, Some(zone)),
                None => (inner, None),
            };
            let ip: Ipv6Addr = ip.parse().map_err(|_| TCP_NOT_V6)?;
            let link_local = ip.segments()[0] & 0xffc0 == 0xfe80;
            let zone = match zone {
                None if link_local => return Err(TCP_NEEDS_ZONE),
                None => None,
                Some(_) if !link_local => return Err(TCP_ZONE_NOT_LINK_LOCAL),
                Some(zone) => {
                    let named = !zone.is_empty()
                        && zone.len() <= ZONE_MAX
                        && zone
                            .bytes()
                            .all(|b| b.is_ascii_graphic() && b != b'/' && b != b'%');
                    if !named {
                        return Err(TCP_ZONE);
                    }
                    Some(zone.to_string())
                }
            };
            (TcpAddr::V6(ip, zone), port)
        } else {
            let (host, port) = rest.rsplit_once(':').ok_or(TCP_SHAPE)?;
            if host.is_empty() {
                return Err(TCP_SHAPE);
            }
            match host.parse::<Ipv4Addr>() {
                Ok(ip) => (TcpAddr::V4(ip), port),
                // More colons: an IPv6 address written bare, where its last group would be
                // read as the port.
                Err(_) if host.contains(':') || host.contains('%') => return Err(TCP_BRACKETS),
                Err(_) => return Err(TCP_HOST_NAME),
            }
        };
        let digits = !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit());
        let port = match port.parse::<u16>() {
            Ok(port) if digits && port != 0 => port,
            _ => return Err(TCP_PORT),
        };
        Ok(Self {
            text: rest.to_string(),
            addr,
            port,
        })
    }

    /// The address to connect to now. A zone is looked up here, at each look: an interface that
    /// is not there yet is an error for this look only.
    fn resolve(&self) -> io::Result<SocketAddr> {
        Ok(match &self.addr {
            TcpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(*ip, self.port)),
            TcpAddr::V6(ip, None) => SocketAddr::V6(SocketAddrV6::new(*ip, self.port, 0, 0)),
            TcpAddr::V6(ip, Some(zone)) => {
                let scope = lemnos_linux_sys::tcp::interface_index(zone)?;
                SocketAddr::V6(SocketAddrV6::new(*ip, self.port, 0, scope))
            }
        })
    }
}

impl Source {
    /// Reads one entry of the `health` list. Anything else is refused by name.
    pub fn parse(text: &str) -> Result<Self, Refused> {
        if let Some(id) = text.strip_prefix("device:") {
            if id.is_empty() {
                return Err(Refused {
                    sentence: format!("`{text}` names no device"),
                    reason: NOT_A_SOURCE,
                });
            }
            return Ok(Self::Device(id.to_string()));
        }
        let path = |rest: &str| {
            if rest.starts_with('/') {
                Ok(PathBuf::from(rest))
            } else {
                Err(Refused {
                    sentence: format!("`{text}` needs an absolute path"),
                    reason: NOT_A_SOURCE,
                })
            }
        };
        if let Some(rest) = text.strip_prefix("file:") {
            return path(rest).map(Self::File);
        }
        if let Some(rest) = text.strip_prefix("socket:") {
            return path(rest).map(Self::Socket);
        }
        if let Some(rest) = text.strip_prefix("tcp:") {
            return Tcp::parse(rest)
                .map(Self::Tcp)
                .map_err(|reason| Refused::new(text, reason));
        }
        Err(Refused {
            sentence: format!(
                "`{text}` is not a health source: device:<id>, file:<path>, socket:<path> or tcp:<address>:<port>"
            ),
            reason: NOT_A_SOURCE,
        })
    }
}

/// A device's health, owned: what the board view gave, kept across the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hs {
    Ok,
    Degraded(String),
    Failed(String),
    Gone(String),
}

/// One device as the board showed it at the last look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dev {
    pub id: String,
    /// From the board file's own devices, not one an `[[each]]` entry made.
    pub builtin: bool,
    pub bound: bool,
    pub health: Hs,
}

impl From<BoardDevice<'_>> for Dev {
    fn from(device: BoardDevice<'_>) -> Self {
        Self {
            id: device.id.to_string(),
            builtin: device.origin == Origin::BuiltIn,
            bound: device.bound,
            health: match device.health {
                Health::Ok => Hs::Ok,
                Health::Degraded(why) => Hs::Degraded(why.to_string()),
                Health::Failed(why) => Hs::Failed(why.to_string()),
                Health::Gone(why) => Hs::Gone(why.to_string()),
            },
        }
    }
}

/// The devices that were running and healthy in a slot: the baseline a trial must not regress.
/// One line per device, `<id> builtin|hotplug`.
pub fn baseline_of(now: &[Dev]) -> String {
    let mut text = String::new();
    for dev in now.iter().filter(|d| d.bound && d.health == Hs::Ok) {
        let origin = if dev.builtin { "builtin" } else { "hotplug" };
        text.push_str(&format!("{} {origin}\n", dev.id));
    }
    text
}

/// A baseline's devices, each with whether it came from the board file.
pub fn parse_baseline(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|line| {
            let (id, origin) = line.split_once(' ')?;
            Some((id.to_string(), origin == "builtin"))
        })
        .collect()
}

/// The first device that was running in the baseline and regressed now, as the reason.
///
/// A device from the board file's own list (on a bus that stays put) regresses when it is missing,
/// not bound, failed or gone. A hot-pluggable device (on a USB port, or made by an `[[each]]`
/// entry) is not the board's own hardware: when it is missing, not bound or gone it is simply not
/// there now, and that is not a regression. Only one that is bound and failed is. A device that is
/// new, or was already failed in the baseline, never counts.
pub fn regression(baseline: &[(String, bool)], now: &[Dev]) -> Option<String> {
    for (id, builtin) in baseline {
        match now.iter().find(|dev| &dev.id == id) {
            // A missing device the board itself names is a regression; a hot-pluggable one is
            // simply not there.
            None if *builtin => {
                return Some(format!(
                    "built-in device {id} was running in the last good slot and is missing here"
                ));
            }
            None => {}
            Some(dev) if !dev.builtin => {
                if let (true, Hs::Failed(why)) = (dev.bound, &dev.health) {
                    return Some(format!(
                        "device {id} was running in the last good slot and is failed here: {why}"
                    ));
                }
            }
            Some(dev) => {
                if !dev.bound {
                    return Some(format!(
                        "built-in device {id} was running in the last good slot and didn't bind here"
                    ));
                }
                match &dev.health {
                    Hs::Failed(why) => {
                        return Some(format!(
                            "device {id} was running in the last good slot and is failed here: {why}"
                        ));
                    }
                    Hs::Gone(why) => {
                        return Some(format!(
                            "device {id} was running in the last good slot and is gone here: {why}"
                        ));
                    }
                    Hs::Ok | Hs::Degraded(_) => {}
                }
            }
        }
    }
    None
}

/// The first `device:` source that isn't Ok now, as the reason.
fn device_unmet(sources: &[Source], now: &[Dev]) -> Option<String> {
    sources.iter().find_map(|source| {
        let Source::Device(id) = source else {
            return None;
        };
        match now.iter().find(|dev| &dev.id == id).map(|dev| &dev.health) {
            None => Some(format!("health source device:{id} is not on the board")),
            Some(Hs::Ok) => None,
            Some(Hs::Degraded(why)) => {
                Some(format!("health source device:{id} is degraded: {why}"))
            }
            Some(Hs::Failed(why)) => Some(format!("health source device:{id} is failed: {why}")),
            Some(Hs::Gone(why)) => Some(format!("health source device:{id} is gone: {why}")),
        }
    })
}

/// How long a `socket:` source may stay busy (its listener's backlog full), or a `tcp:` source's
/// connect stay unanswered, before it is not good.
pub const SOCKET_DEADLINE_NS: u64 = 2_000_000_000;

/// What a `socket:` or `tcp:` source's check remembers between looks: since when it has been
/// busy, and for `tcp:` the connect a look left on its way, with when it began.
#[derive(Debug, Default)]
pub struct Probe {
    busy_since: Option<u64>,
    pending: Option<(OwnedFd, u64)>,
}

/// The first source that isn't good now, as the reason: device health from the board view, file
/// and socket checks made here. `boot` is when this boot began. `probes` holds one entry per
/// source, kept between looks; `now_ns` is the monotonic clock. A socket that takes no connection
/// right away is busy, and only that look's answer is not good: it is asked again at the next
/// look, and it is not good for good once it has been busy for [`SOCKET_DEADLINE_NS`]. A TCP
/// port is the same, see [`tcp_unmet`].
pub fn source_unmet(
    sources: &[Source],
    probes: &mut Vec<Probe>,
    now: &[Dev],
    boot: SystemTime,
    now_ns: u64,
) -> Option<String> {
    if let Some(reason) = device_unmet(sources, now) {
        return Some(reason);
    }
    probes.resize_with(sources.len(), Probe::default);
    sources
        .iter()
        .zip(probes.iter_mut())
        .find_map(|(source, probe)| match source {
            Source::Device(_) => None,
            Source::File(path) => file_unmet(path, boot),
            Source::Socket(path) => socket_unmet(path, probe, now_ns),
            Source::Tcp(tcp) => tcp_unmet(tcp, probe, now_ns),
        })
}

fn file_unmet(path: &Path, boot: SystemTime) -> Option<String> {
    let Ok(meta) = fs::metadata(path) else {
        return Some(format!(
            "health source file:{} does not exist",
            path.display()
        ));
    };
    match meta.modified() {
        Ok(modified) if modified >= boot => None,
        _ => Some(format!(
            "health source file:{} was not written since this boot",
            path.display()
        )),
    }
}

/// One look at a socket: a connect that does not wait. See [`source_unmet`].
fn socket_unmet(path: &Path, probe: &mut Probe, now_ns: u64) -> Option<String> {
    match lemnos_linux_sys::unix::connect_nonblocking(path) {
        Ok(lemnos_linux_sys::unix::Connect::Connected) => {
            probe.busy_since = None;
            None
        }
        Ok(lemnos_linux_sys::unix::Connect::Busy) => {
            let since = *probe.busy_since.get_or_insert(now_ns);
            if now_ns.saturating_sub(since) >= SOCKET_DEADLINE_NS {
                Some(format!(
                    "health source socket:{} did not accept a connection within {} s (its backlog stays full)",
                    path.display(),
                    SOCKET_DEADLINE_NS / 1_000_000_000
                ))
            } else {
                Some(format!(
                    "health source socket:{} is busy: its backlog is full",
                    path.display()
                ))
            }
        }
        Err(error) => {
            probe.busy_since = None;
            Some(format!(
                "health source socket:{} doesn't accept a connection: {error}",
                path.display()
            ))
        }
    }
}

/// One look at a TCP port: a connect that does not wait, and no byte sent. A connect to an
/// address of this board is answered at once, taken or refused. One that is not (the address is
/// not on the board yet, so the connect went out on a link) is busy: it is kept, and the next
/// look asks it again. A connect is given up [`SOCKET_DEADLINE_NS`] after it began and the next
/// look makes a new one, so a port that comes up later is seen at the next look, not at the
/// kernel's next retry. Every error (refused, no route, no such address, a zone whose interface
/// is not there yet) is this look's answer only.
fn tcp_unmet(tcp: &Tcp, probe: &mut Probe, now_ns: u64) -> Option<String> {
    use lemnos_linux_sys::tcp::{Connect, connect_nonblocking, finished};

    let answer = match probe.pending.take() {
        Some((fd, began)) => match finished(fd.as_fd()) {
            Ok(true) => Ok(Connect::Connected),
            Ok(false) if now_ns.saturating_sub(began) >= SOCKET_DEADLINE_NS => {
                // Given up: closed here, and the next look connects again.
                return Some(tcp_busy(tcp, probe, now_ns));
            }
            Ok(false) => {
                probe.pending = Some((fd, began));
                return Some(tcp_busy(tcp, probe, now_ns));
            }
            Err(error) => Err(error),
        },
        None => tcp.resolve().and_then(|addr| connect_nonblocking(&addr)),
    };
    match answer {
        Ok(Connect::Connected) => {
            probe.busy_since = None;
            None
        }
        Ok(Connect::Pending(fd)) => {
            probe.pending = Some((fd, now_ns));
            Some(tcp_busy(tcp, probe, now_ns))
        }
        Err(error) => {
            probe.busy_since = None;
            Some(format!(
                "health source tcp:{} doesn't accept a connection: {error}",
                tcp.text
            ))
        }
    }
}

/// The reason for a `tcp:` source whose connect has no answer yet, by how long that has been so.
fn tcp_busy(tcp: &Tcp, probe: &mut Probe, now_ns: u64) -> String {
    let since = *probe.busy_since.get_or_insert(now_ns);
    if now_ns.saturating_sub(since) >= SOCKET_DEADLINE_NS {
        format!(
            "health source tcp:{} did not accept a connection within {} s (its connect stays unanswered)",
            tcp.text,
            SOCKET_DEADLINE_NS / 1_000_000_000
        )
    } else {
        format!(
            "health source tcp:{} is busy: its connect has no answer yet",
            tcp.text
        )
    }
}

/// The trial's own record on the state directory: `board/slots/trial.env`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    /// The trial slot.
    pub slot: String,
    /// The attempts the driver last left the loader's counter at (the attempts it had before the
    /// current boot took one). A boot that never reached the daemon shows as a lower count.
    pub seen: u8,
    /// What the last boot did: one of the four words, `""` before any boot.
    pub last: String,
    pub last_time: String,
    /// Why it fell back, or why the board is asking for a restart at the deadline.
    pub reason: String,
    /// The boot that gave its attempt back, so one boot gives back at most one.
    pub boot: String,
    /// Each attempt, numbered from 1, with when it was seen.
    pub attempts: Vec<(u32, String)>,
}

impl Log {
    fn path(layout: &Layout) -> PathBuf {
        layout.marker("trial.env")
    }

    pub fn load(layout: &Layout) -> Option<Self> {
        let text = fs::read_to_string(Self::path(layout)).ok()?;
        let mut log = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "SLOT" => log.slot = value.to_string(),
                "SEEN" => log.seen = value.parse().unwrap_or(0),
                "LAST" => log.last = value.to_string(),
                "LAST_TIME" => log.last_time = value.to_string(),
                "REASON" => log.reason = value.to_string(),
                "BOOT" => log.boot = value.to_string(),
                "ATTEMPT" => {
                    if let Some((n, time)) = value.split_once(' ') {
                        log.attempts
                            .push((n.parse().unwrap_or(0), time.to_string()));
                    }
                }
                _ => {}
            }
        }
        (!log.slot.is_empty()).then_some(log)
    }

    pub fn save(&self, layout: &Layout) -> io::Result<()> {
        fs::create_dir_all(layout.state_dir())?;
        let mut text = String::new();
        let line = |text: &mut String, key: &str, value: &str| {
            text.push_str(key);
            text.push('=');
            text.push_str(&value.replace(['\n', '\r'], " "));
            text.push('\n');
        };
        line(&mut text, "SLOT", &self.slot);
        line(&mut text, "SEEN", &self.seen.to_string());
        line(&mut text, "LAST", &self.last);
        line(&mut text, "LAST_TIME", &self.last_time);
        line(&mut text, "REASON", &self.reason);
        line(&mut text, "BOOT", &self.boot);
        for (n, time) in &self.attempts {
            line(&mut text, "ATTEMPT", &format!("{n} {time}"));
        }
        replace(&Self::path(layout), text.as_bytes())
    }

    pub fn remove(layout: &Layout) {
        let _ = fs::remove_file(Self::path(layout));
    }

    /// Records one boot's outcome: `last` at the time now.
    pub fn note(&mut self, last: &str) {
        self.last = last.to_string();
        self.last_time = now_utc();
    }

    /// Adds an attempt, numbered after the ones there are.
    pub fn add_attempt(&mut self) {
        let n = self.attempts.len() as u32 + 1;
        self.attempts.push((n, now_utc()));
    }

    /// The `trial` field of `list`, given the attempts the loader holds now and whether the
    /// driver could not write its record of the trial (`record_error`, the reason it gave).
    pub fn json(&self, attempts_left: u8, record_error: Option<&str>) -> String {
        let mut out = String::from("{\"slot\":");
        quoted(&self.slot, &mut out);
        out.push_str(&format!(
            ",\"attempts_left\":{attempts_left},\"attempts\":["
        ));
        for (at, (n, time)) in self.attempts.iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"n\":{n},\"time\":"));
            quoted(time, &mut out);
            out.push('}');
        }
        out.push_str("],\"last\":");
        optional(&self.last, &mut out);
        out.push_str(",\"last_time\":");
        optional(&self.last_time, &mut out);
        out.push_str(",\"reason\":");
        optional(&self.reason, &mut out);
        out.push_str(",\"record_error\":");
        match record_error {
            Some(error) => quoted(error, &mut out),
            None => out.push_str("null"),
        }
        out.push('}');
        out
    }
}

/// What resolved last: `board/slots/resolved.env`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub slot: String,
    /// `confirmed` or `fell-back`.
    pub outcome: String,
    /// `auto` or `call`.
    pub by: String,
    /// The caller of a `call`, `None` for `auto`.
    pub caller: Option<u64>,
    pub reason: String,
    pub time: String,
}

impl Resolved {
    fn path(layout: &Layout) -> PathBuf {
        layout.marker("resolved.env")
    }

    pub fn load(layout: &Layout) -> Option<Self> {
        let text = fs::read_to_string(Self::path(layout)).ok()?;
        let mut out = Self {
            slot: String::new(),
            outcome: String::new(),
            by: String::new(),
            caller: None,
            reason: String::new(),
            time: String::new(),
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "SLOT" => out.slot = value.to_string(),
                "OUTCOME" => out.outcome = value.to_string(),
                "BY" => out.by = value.to_string(),
                "CALLER" => out.caller = value.parse().ok(),
                "REASON" => out.reason = value.to_string(),
                "TIME" => out.time = value.to_string(),
                _ => {}
            }
        }
        (!out.slot.is_empty()).then_some(out)
    }

    pub fn save(&self, layout: &Layout) -> io::Result<()> {
        fs::create_dir_all(layout.state_dir())?;
        let caller = self.caller.map(|c| c.to_string()).unwrap_or_default();
        let text = format!(
            "SLOT={}\nOUTCOME={}\nBY={}\nCALLER={caller}\nREASON={}\nTIME={}\n",
            self.slot,
            self.outcome,
            self.by,
            self.reason.replace(['\n', '\r'], " "),
            self.time
        );
        replace(&Self::path(layout), text.as_bytes())?;
        // The boot history's entry for this trial takes the same outcome (`lemnos/boots`).
        let _ = lemnos_slot_store::boots::set_outcome(layout, &self.slot, &self.outcome);
        Ok(())
    }

    /// The `resolved` field of `list`.
    pub fn json(&self) -> String {
        let mut out = String::from("{\"slot\":");
        quoted(&self.slot, &mut out);
        out.push_str(",\"outcome\":");
        quoted(&self.outcome, &mut out);
        out.push_str(",\"by\":");
        quoted(&self.by, &mut out);
        out.push_str(",\"caller\":");
        match self.caller {
            Some(caller) => out.push_str(&caller.to_string()),
            None => out.push_str("null"),
        }
        out.push_str(",\"reason\":");
        optional(&self.reason, &mut out);
        out.push_str(",\"time\":");
        quoted(&self.time, &mut out);
        out.push('}');
        out
    }
}

fn optional(value: &str, out: &mut String) {
    if value.is_empty() {
        out.push_str("null");
    } else {
        quoted(value, out);
    }
}
