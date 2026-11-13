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
}

impl Source {
    /// Reads one entry of the `health` list. Anything else is refused by name.
    pub fn parse(text: &str) -> Result<Self, String> {
        if let Some(id) = text.strip_prefix("device:") {
            if id.is_empty() {
                return Err(format!("`{text}` names no device"));
            }
            return Ok(Self::Device(id.to_string()));
        }
        let path = |rest: &str| {
            if rest.starts_with('/') {
                Ok(PathBuf::from(rest))
            } else {
                Err(format!("`{text}` needs an absolute path"))
            }
        };
        if let Some(rest) = text.strip_prefix("file:") {
            return path(rest).map(Self::File);
        }
        if let Some(rest) = text.strip_prefix("socket:") {
            return path(rest).map(Self::Socket);
        }
        Err(format!(
            "`{text}` is not a health source: device:<id>, file:<path> or socket:<path>"
        ))
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

/// The first device that was running in the baseline and fails now, as the reason. A device
/// that is absent is failed only when it came from the board file: a hotplug device that is
/// simply not there is not, and neither is one that is Gone (its resource is dead and not
/// failing) or a new device. A built-in device that is present and not bound counts too.
pub fn regression(baseline: &[(String, bool)], now: &[Dev]) -> Option<String> {
    for (id, builtin) in baseline {
        match now.iter().find(|dev| &dev.id == id) {
            None if *builtin => {
                return Some(format!(
                    "built-in device {id} was running in the last good slot and is missing here"
                ));
            }
            None => {}
            Some(dev) => {
                if *builtin && !dev.bound {
                    return Some(format!(
                        "built-in device {id} was running in the last good slot and didn't bind here"
                    ));
                }
                if let Hs::Failed(why) = &dev.health {
                    return Some(format!(
                        "device {id} was running in the last good slot and is failed here: {why}"
                    ));
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

/// The first source that isn't good now, as the reason: device health from the board view, file
/// and socket checks made here. `boot` is when this boot began.
pub fn source_unmet(sources: &[Source], now: &[Dev], boot: SystemTime) -> Option<String> {
    if let Some(reason) = device_unmet(sources, now) {
        return Some(reason);
    }
    sources.iter().find_map(|source| match source {
        Source::Device(_) => None,
        Source::File(path) => file_unmet(path, boot),
        Source::Socket(path) => socket_unmet(path),
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

fn socket_unmet(path: &Path) -> Option<String> {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => None,
        Err(error) => Some(format!(
            "health source socket:{} doesn't accept a connection: {error}",
            path.display()
        )),
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

    /// The `trial` field of `list`, given the attempts the loader holds now.
    pub fn json(&self, attempts_left: u8) -> String {
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
        replace(&Self::path(layout), text.as_bytes())
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
